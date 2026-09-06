"""API Gateway — Feedback loop routes"""

import json
import logging
from typing import Optional

from fastapi import APIRouter, HTTPException, Query
from pydantic import BaseModel

from api_gateway.services.db import get_connection

logger = logging.getLogger("api_gateway.feedback")

router = APIRouter()


class FeedbackCreate(BaseModel):
    ai_analysis_id: str
    user_id: Optional[str] = None
    rating: Optional[int] = None
    accepted: Optional[bool] = None
    user_edits: Optional[dict] = None
    action_taken: Optional[str] = None
    was_paid_on_resubmit: Optional[bool] = None
    resubmit_result: Optional[str] = None
    feedback_text: Optional[str] = None


@router.get("/feedback", response_model=list[dict])
async def list_feedback(
    ai_analysis_id: str = Query(None),
    accepted: bool = Query(None),
    limit: int = Query(50),
):
    """List feedback records"""
    async with get_connection() as conn:
        query = """
            SELECT fl.*, aa.explanation, aa.denial_category,
                   c.claim_number, c.patient_name
            FROM feedback_loop fl
            JOIN ai_analyses aa ON aa.id = fl.ai_analysis_id
            JOIN denials d ON d.id = aa.denial_id
            JOIN claims c ON c.id = aa.claim_id
        """
        params = []
        where_count = 0

        filters = []
        if ai_analysis_id:
            filters.append(f"fl.ai_analysis_id = ${where_count + 1}")
            params.append(ai_analysis_id)
            where_count += 1
        if accepted is not None:
            filters.append(f"fl.accepted = ${where_count + 1}")
            params.append(accepted)
            where_count += 1

        if filters:
            query += " WHERE " + " AND ".join(filters)

        query += " ORDER BY fl.created_at DESC LIMIT $%d" % (where_count + 1)
        params.append(limit)

        rows = await conn.fetch(query, *params)
        return [dict(r) for r in rows]


@router.post("/feedback", response_model=dict, status_code=201)
async def create_feedback(feedback: FeedbackCreate):
    """Submit feedback on an AI analysis"""
    async with get_connection() as conn:
        row = await conn.fetchrow(
            """
            INSERT INTO feedback_loop
                (ai_analysis_id, user_id, rating, accepted, user_edits, action_taken,
                 was_paid_on_resubmit, resubmit_result, feedback_text)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            RETURNING *
            """,
            feedback.ai_analysis_id,
            feedback.user_id,
            feedback.rating,
            feedback.accepted,
            json.dumps(feedback.user_edits) if feedback.user_edits else None,
            feedback.action_taken,
            feedback.was_paid_on_resubmit,
            feedback.resubmit_result,
            feedback.feedback_text,
        )
        return dict(row)


@router.get("/feedback/analytics")
async def feedback_analytics():
    """Aggregate how well the AI's recommendations actually performed.

    Rates are reported against honest denominators: an outcome is only known
    once someone recorded whether the resubmission was paid, so the success
    rate divides by rows where `was_paid_on_resubmit IS NOT NULL` rather than
    by all feedback. Dividing by everything would understate accuracy purely
    because work is still in flight.
    """
    async with get_connection() as conn:
        head = await conn.fetchrow("""
            SELECT
                COUNT(*)                                                   AS total_feedback,
                COUNT(*) FILTER (WHERE accepted)                           AS accepted_count,
                COUNT(*) FILTER (WHERE accepted IS NOT NULL)               AS rated_accept_count,
                COUNT(*) FILTER (WHERE was_paid_on_resubmit)               AS success_count,
                COUNT(*) FILTER (WHERE was_paid_on_resubmit IS NOT NULL)   AS outcome_known_count,
                AVG(rating)                                                AS avg_rating,
                COUNT(*) FILTER (WHERE action_taken = 'corrected_claim')   AS corrected_count
            FROM feedback_loop
        """)

        by_action = await conn.fetch("""
            SELECT
                COALESCE(aa.required_action, 'unclassified')             AS required_action,
                COUNT(*)                                                 AS feedback_count,
                AVG(fl.rating)                                           AS avg_rating,
                COUNT(*) FILTER (WHERE fl.was_paid_on_resubmit)          AS paid_count,
                COUNT(*) FILTER (WHERE fl.was_paid_on_resubmit IS NOT NULL) AS outcome_known
            FROM feedback_loop fl
            JOIN ai_analyses aa ON aa.id = fl.ai_analysis_id
            GROUP BY COALESCE(aa.required_action, 'unclassified')
            ORDER BY COUNT(*) DESC
        """)

        by_carc = await conn.fetch("""
            SELECT
                d.carc_code,
                COALESCE(cc.description, 'No description on file')       AS carc_description,
                COUNT(*)                                                 AS feedback_count,
                AVG(fl.rating)                                           AS avg_rating,
                COUNT(*) FILTER (WHERE fl.was_paid_on_resubmit)          AS paid_count,
                COUNT(*) FILTER (WHERE fl.was_paid_on_resubmit IS NOT NULL) AS outcome_known
            FROM feedback_loop fl
            JOIN ai_analyses aa ON aa.id = fl.ai_analysis_id
            JOIN denials d ON d.id = aa.denial_id
            LEFT JOIN carc_codes cc ON cc.code = d.carc_code
            GROUP BY d.carc_code, cc.description
            ORDER BY COUNT(*) DESC
            LIMIT 10
        """)

        # Which payers the model reads well, and which it does not. A model
        # that is right about Medicare and wrong about one commercial plan is
        # a different problem from one that is uniformly mediocre, and the
        # aggregate number hides the difference.
        by_payer = await conn.fetch("""
            SELECT
                COALESCE(c.payer_name, 'Unknown')                        AS payer_name,
                COUNT(*)                                                 AS feedback_count,
                AVG(fl.rating)                                           AS avg_rating,
                COUNT(*) FILTER (WHERE fl.was_paid_on_resubmit)          AS paid_count,
                COUNT(*) FILTER (WHERE fl.was_paid_on_resubmit IS NOT NULL) AS outcome_known,
                SUM(d.charge_amount) FILTER (WHERE fl.was_paid_on_resubmit) AS recovered_amount
            FROM feedback_loop fl
            JOIN ai_analyses aa ON aa.id = fl.ai_analysis_id
            JOIN denials d ON d.id = aa.denial_id
            JOIN claims c ON c.id = d.claim_id
            GROUP BY COALESCE(c.payer_name, 'Unknown')
            ORDER BY COUNT(*) DESC
            LIMIT 10
        """)

        # Month by month, so "is it getting better" is answerable. A single
        # lifetime average cannot distinguish a model that has improved from
        # one that never worked.
        trend = await conn.fetch("""
            SELECT
                date_trunc('month', fl.created_at)::date                 AS month,
                COUNT(*)                                                 AS feedback_count,
                AVG(fl.rating)                                           AS avg_rating,
                COUNT(*) FILTER (WHERE fl.was_paid_on_resubmit)          AS paid_count,
                COUNT(*) FILTER (WHERE fl.was_paid_on_resubmit IS NOT NULL) AS outcome_known
            FROM feedback_loop fl
            WHERE fl.created_at > NOW() - INTERVAL '12 months'
            GROUP BY 1 ORDER BY 1
        """)

        # What the recommendations were actually worth, in money. Ratings say
        # whether a biller liked the advice; this says whether it got paid.
        money = await conn.fetchrow("""
            SELECT
                COALESCE(SUM(d.charge_amount) FILTER (WHERE fl.was_paid_on_resubmit), 0) AS recovered,
                COALESCE(SUM(d.charge_amount) FILTER (WHERE fl.was_paid_on_resubmit IS FALSE), 0) AS not_recovered,
                COALESCE(SUM(d.charge_amount) FILTER (WHERE fl.was_paid_on_resubmit IS NULL), 0) AS still_open
            FROM feedback_loop fl
            JOIN ai_analyses aa ON aa.id = fl.ai_analysis_id
            JOIN denials d ON d.id = aa.denial_id
        """)

        # How many analyses have been reviewed at all - the coverage of the
        # feedback loop itself, which is what tells you whether these rates
        # are worth anything yet.
        total_analyses = await conn.fetchval("SELECT COUNT(*) FROM ai_analyses")

    def _rate(numerator, denominator):
        return round(float(numerator) / denominator, 4) if denominator else None

    return {
        "total_analyses": total_analyses,
        "total_feedback": head["total_feedback"],
        "coverage_rate": _rate(head["total_feedback"], total_analyses),
        "accepted_count": head["accepted_count"],
        "acceptance_rate": _rate(head["accepted_count"], head["rated_accept_count"]),
        "success_count": head["success_count"],
        "outcome_known_count": head["outcome_known_count"],
        "success_rate": _rate(head["success_count"], head["outcome_known_count"]),
        "avg_rating": float(head["avg_rating"]) if head["avg_rating"] is not None else None,
        "corrected_count": head["corrected_count"],
        "by_required_action": [
            {
                "required_action": r["required_action"],
                "feedback_count": r["feedback_count"],
                "avg_rating": float(r["avg_rating"]) if r["avg_rating"] is not None else None,
                "paid_count": r["paid_count"],
                "outcome_known": r["outcome_known"],
                "success_rate": _rate(r["paid_count"], r["outcome_known"]),
            }
            for r in by_action
        ],
        "by_carc": [
            {
                "carc_code": r["carc_code"],
                "carc_description": r["carc_description"],
                "feedback_count": r["feedback_count"],
                "avg_rating": float(r["avg_rating"]) if r["avg_rating"] is not None else None,
                "paid_count": r["paid_count"],
                "outcome_known": r["outcome_known"],
                "success_rate": _rate(r["paid_count"], r["outcome_known"]),
            }
                  for r in by_carc
            ],
            "by_payer": [
                {
                    "payer_name": r["payer_name"],
                    "feedback_count": r["feedback_count"],
                    "avg_rating": float(r["avg_rating"]) if r["avg_rating"] is not None else None,
                    "paid_count": r["paid_count"],
                    "outcome_known": r["outcome_known"],
                    "success_rate": _rate(r["paid_count"], r["outcome_known"]),
                    "recovered_amount": float(r["recovered_amount"] or 0),
                }
                for r in by_payer
            ],
            "trend": [
                {
                    "month": r["month"].isoformat(),
                    "feedback_count": r["feedback_count"],
                    "avg_rating": float(r["avg_rating"]) if r["avg_rating"] is not None else None,
                    "paid_count": r["paid_count"],
                    "outcome_known": r["outcome_known"],
                    "success_rate": _rate(r["paid_count"], r["outcome_known"]),
                }
                for r in trend
            ],
            "money": {
                "recovered": float(money["recovered"]),
                "not_recovered": float(money["not_recovered"]),
                "still_open": float(money["still_open"]),
            },
        }
