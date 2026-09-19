# ADR-0002: Persist safe RAG citation metadata with AI analyses

## Status

Accepted

## Context

Denial recommendations can use organization-scoped RAG documents, but the
stored analysis did not identify which retrieved evidence the model cited.
Persisting raw chunks or excerpts would duplicate potentially sensitive source
content in the analysis record.

## Decision

Store a JSONB citation list on each AI analysis containing only the cited chunk
ID, document ID, source type, and chunk index. The LLM service passes the
validated model result and retrieval allowlist to the gateway. The gateway
derives cited IDs from that result, rejects IDs outside the allowlist, and
resolves the stable metadata from organization-scoped knowledge tables before
inserting the analysis. Denial reads hydrate document titles separately from
organization-scoped knowledge documents.

## Consequences

Analysis details can display traceable references without storing chunk content
or document titles. The gateway performs one scoped lookup when storing cited
metadata and another when reading citations. Existing analyses and deterministic
fallback analyses have an empty citation list.
