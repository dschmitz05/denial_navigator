"""API Gateway — AI Analyses routes"""

import json
import logging
from uuid import UUID
from typing import Optional

from fastapi import APIRouter, HTTPException, Query, Request
from pydantic import BaseModel

from api_gateway.services.db import get_connection
from api_gateway.services import RAGEngineClient, LLMServiceClient
from api_gateway.services.audit import _client_ip, _identify, record as audit_record

logger = logging.getLogger("api_gateway.analyses")

rag_client = RAGEngineClient()
llm_client = LLMServiceClient()

router = APIRouter()


class StoreAnalysis(BaseModel):
    denial_id: str
    claim_id: str
    model_name: str
    raw_prompt: str
    raw_response: str
    parsed_result: dict
    prompt_tokens: int = 0
    completion_tokens: int = 0
    total_tokens: int = 0


class GenerateAnalysisRequest(BaseModel):
    denial_id: str
    temperature: float = 0.3


@router.get("/analyses", response_model=list[dict])
async def list_analyses(
    denial_id: Optional[UUID] = Query(None),
    claim_id: str = Query(None),
    limit: int = Query(50),
):
    """List AI analyses"""
    async with get_connection() as conn:
        query = """
            SELECT aa.*, d.cpt_code, d.carc_code,
                   c.claim_number, c.patient_name, c.payer_name
            FROM ai_analyses aa
            JOIN denials d ON d.id = aa.denial_id
            JOIN claims c ON c.id = aa.claim_id
        """
        params = []
        where_count = 0

        filters = []
        if denial_id:
            filters.append(f"aa.denial_id = ${where_count + 1}")
            params.append(denial_id)
            where_count += 1
        if claim_id:
            filters.append(f"aa.claim_id = ${where_count + 1}")
            params.append(claim_id)
            where_count += 1

        if filters:
            query += " WHERE " + " AND ".join(filters)

        query += " ORDER BY aa.created_at DESC LIMIT $%d" % (where_count + 1)
        params.append(limit)

        rows = await conn.fetch(query, *params)
        return [dict(r) for r in rows]


@router.post("/analyses/store", response_model=dict)
async def store_analysis(analysis: StoreAnalysis):
    """Store an AI analysis result"""
    async with get_connection() as conn:
        pr = analysis.parsed_result or {}

        row = await conn.fetchrow(
            """
            INSERT INTO ai_analyses
                (denial_id, claim_id, model_name, prompt_tokens, completion_tokens, total_tokens,
                 system_prompt_template, raw_prompt, raw_response,
                 explanation, denial_category, required_action, root_cause_summary, action_plan, steps,
                 needs_appeal, draft_appeal_letter, confidence_score)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9,
                    $10, $11, $12, $13, $14, $15,
                    $16, $17, $18)
            RETURNING *
            """,
            analysis.denial_id, analysis.claim_id, analysis.model_name,
            analysis.prompt_tokens, analysis.completion_tokens, analysis.total_tokens,
            "denial_analysis_v1", analysis.raw_prompt, analysis.raw_response,
            pr.get("explanation"),
            pr.get("denial_category"),
            pr.get("required_action"),
            pr.get("root_cause_summary"),
            json.dumps(pr.get("action_plan")),
            json.dumps(pr.get("steps")),
            pr.get("needs_appeal", False),
            pr.get("draft_appeal_letter", ""),
            float(pr.get("confidence_score", 0.0)),
        )
        return dict(row)


@router.post("/analyses/generate", response_model=dict)
async def generate_analysis(request: GenerateAnalysisRequest, http_request: Request):
    """Generate a complete AI analysis for a denial (RAG + LLM)"""
    async with get_connection() as conn:
        denial = await conn.fetchrow(
            """
            SELECT d.*, c.claim_number, c.patient_name, c.payer_name,
                   c.icd_10_codes, cc.description as carc_description,
                   rc.description as rarc_description
            FROM denials d
            JOIN claims c ON c.id = d.claim_id
            LEFT JOIN carc_codes cc ON cc.code = d.carc_code
            LEFT JOIN rarc_codes rc ON rc.code = d.rarc_code
            WHERE d.id = $1
            """,
            request.denial_id,
        )

        if not denial:
            raise HTTPException(status_code=404, detail="Denial not found")

        # All of them, not just the first. Medical-necessity denials usually
        # turn on a secondary diagnosis - the one that justifies the service -
        # and searching on icd_10_codes[0] alone dropped exactly the code the
        # policy would have been found by. Capped so a long list cannot drown
        # out the CPT and CARC terms.
        icd = " ".join((denial.get("icd_10_codes") or [])[:5])
        search_query = f"{denial['payer_name']} {denial['cpt_code']} {icd} {denial['carc_code']}"

        try:
            search_results = await rag_client.search(search_query, top_k=5, filters={"payer": denial["payer_name"]})
            policy_texts = [r.get("content", "") for r in search_results.get("results", [])]
        except Exception as e:
            logger.warning(f"RAG search failed: {e}")
            policy_texts = []

        prompt = await rag_client.build_prompt(
            claim_id=denial["claim_number"],
            payer_name=denial["payer_name"],
            cpt_code=denial["cpt_code"] or "",
            icd10_code=icd,
            cagc=denial["cagc"],
            carc_code=denial["carc_code"] or "",
            carc_definition=denial["carc_description"] or "Unknown",
            rarc_code=denial["rarc_code"] or "",
            rarc_definition=denial["rarc_description"] or "Unknown",
            retrieved_policies=policy_texts,
        )

        llm_request = {
            "denial_id": request.denial_id,
            # ai_analyses.claim_id is a UUID foreign key to claims(id). This
            # passed the human-readable claim NUMBER, so every store failed
            # with "invalid UUID 'PCN10001'". The claim number is already
            # baked into the prompt text above, where it belongs.
            "claim_id": str(denial["claim_id"]),
            "prompt": prompt,
            "temperature": request.temperature,
        }

        llm_result = await llm_client.analyze_denial(llm_request)

        # The denial id arrives in the body, so the access log sees no record
        # in the path to name. This entry says whose claim was analysed.
        actor_id, actor_name = _identify(http_request)
        await audit_record(
            action="generate_analysis",
            resource_type="denial",
            resource_id=str(request.denial_id),
            user_id=actor_id,
            details={
                "username": actor_name or "anonymous",
                "claim_number": denial["claim_number"],
                "patient_name": denial["patient_name"],
                "carc_code": denial["carc_code"],
                "cpt_code": denial["cpt_code"],
                "policies_retrieved": len(policy_texts),
            },
            ip_address=_client_ip(http_request),
            user_agent=http_request.headers.get("user-agent"),
        )

        return {
            "denial_id": request.denial_id,
            "claim_id": denial["claim_number"],
            "model": llm_result.get("model"),
            "parsed_json": llm_result.get("parsed_json"),
            "stored": llm_result.get("stored"),
            "policy_documents_retrieved": len(policy_texts),
        }
