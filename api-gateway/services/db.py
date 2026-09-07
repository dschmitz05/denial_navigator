"""API Gateway — Database connection management"""

import os
from contextlib import asynccontextmanager
from typing import Optional

import asyncpg

DATABASE_URL = os.environ.get("DATABASE_URL", "postgresql://denial_nav:denial_nav_pass@postgres:5432/denial_navigator")

_pool: Optional[asyncpg.Pool] = None


async def get_pool() -> asyncpg.Pool:
    """Get or create the database connection pool.

    `is_closing()` is the public form of the check; `_pool._closed` was a
    private attribute that asyncpg is free to rename in any release, and it
    would have failed with an AttributeError on every request if it did.
    """
    global _pool
    if _pool is None or _pool.is_closing():
        _pool = await asyncpg.create_pool(DATABASE_URL, min_size=2, max_size=20)
    return _pool


@asynccontextmanager
async def get_connection():
    """Get a database connection from the pool (use as async context manager)"""
    pool = await get_pool()
    conn = await pool.acquire()
    try:
        yield conn
    finally:
        await pool.release(conn)


async def close_db():
    """Close the database pool on shutdown.

    There is no init_db: the app has no lifespan handler, and get_pool()
    creates the pool lazily on first use. The old init_db() was never wired to
    anything and only offered a second, divergent place to configure the pool.
    """
    global _pool
    if _pool:
        await _pool.close()
        _pool = None
