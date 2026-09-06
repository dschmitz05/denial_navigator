"""API Gateway — claim status roll-up.

A claim's status is derived from its denials, not set by hand. Ingestion marks
a claim 'denied' or 'partially_paid' when denials arrive, but nothing used to
walk that back: closing every denial out through Appeals or the Worklist left
the claim reading 'denied' forever, so the Claims tab disagreed with the two
queues that had actually done the work.

The rule, in one place because three routes need it:

  * No denials at all      -> leave the claim alone (nothing to derive from).
  * Every denial closed    -> 'resolved'.
  * Any denial still open  -> back to the adjudicated state, 'partially_paid'
                              when the payer paid something, else 'denied'.

The last case matters as much as the first: cancelling an appeal or a payer
denying again reopens the denial, and the claim has to reopen with it.
"""

from typing import Optional

# A denial nobody has to touch again. Mirrors the terminal outcomes in
# routes/appeals.py, translated to the denial's own vocabulary.
TERMINAL_DENIAL_STATUSES = ("appealed", "overruled", "resolved", "written_off")


async def refresh_claim_status(conn, claim_id) -> Optional[str]:
    """Recompute one claim's status from its denials. Returns the new status.

    Returns None when the claim has no denials, or does not exist.
    """
    return await conn.fetchval(
        """
        UPDATE claims c
           SET status = CASE
                   WHEN s.open_denials = 0 THEN 'resolved'
                   WHEN c.total_paid > 0   THEN 'partially_paid'
                   ELSE 'denied'
               END,
               updated_at = NOW()
          FROM (
               SELECT COUNT(*) AS total,
                      COUNT(*) FILTER (
                          WHERE d.status <> ALL($2::text[])
                      ) AS open_denials
                 FROM denials d
                WHERE d.claim_id = $1
               ) s
         WHERE c.id = $1
           AND s.total > 0
        RETURNING c.status
        """,
        claim_id, list(TERMINAL_DENIAL_STATUSES),
    )
