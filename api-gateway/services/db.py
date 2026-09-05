"""API Gateway — Database connection management"""

import os
from contextlib import asynccontextmanager
from typing import Optional

import asyncpg

DATABASE_URL = os.environ.get("DATABASE_URL", "postgresql://denial_nav:denial_nav_pass@postgres:5432/denial_navigator")

_pool: Optional[asyncpg.Pool] = None


async def get_pool() -> asyncpg.Pool:
    """Get or create the database connection pool"""
    global _pool
    if _pool is None or _pool._closed:
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


async def init_db():
    """Initialize the database pool on startup"""
    global _pool
    _pool = await asyncpg.create_pool(DATABASE_URL, min_size=2, max_size=20)
    print("Database pool initialized")


async def close_db():
    """Close the database pool on shutdown"""
    global _pool
    if _pool:
        await _pool.close()
        print("Database pool closed")
