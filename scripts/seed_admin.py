#!/usr/bin/env python3
"""Seed script to create default admin user and seed data."""
import asyncio
import bcrypt
import os
import asyncpg

DATABASE_URL = os.environ.get(
    "DATABASE_URL",
    "postgresql://denial_nav:denial_nav_pass@localhost:5432/denial_navigator"
)

# Default admin credentials
DEFAULT_USERNAME = "admin"
DEFAULT_EMAIL = "admin@denialnavigator.local"
DEFAULT_PASSWORD = "admin123"  # Change immediately after first login
DEFAULT_ROLE = "system_admin"

# Seed CARC codes
CARC_CODES = [
    ("1", "Service/Supplies not covered by plan/carrier", "payer_policy"),
    ("2", "Duplicate claim/service", "coding_error"),
    ("3", "Medical necessity", "medical_necessity"),
    ("4", "Non-covered diagnosis", "non_covered_service"),
    ("5", "Not a covered benefit", "non_covered_service"),
    ("6", "Experimental/investigational", "non_covered_service"),
    ("7", "Bundle/related services billed separately", "bundled_service"),
    ("8", "Missing/NCCI edit", "coding_error"),
    ("9", "Unlisted procedure/code", "coding_error"),
    ("10", "Incorrect filing channel", "administrative"),
    ("11", "Incorrect patient information", "administrative"),
    ("12", "Incorrect patient status", "administrative"),
    ("13", "Incorrect date", "administrative"),
    ("14", "Incorrect diagnosis", "coding_error"),
    ("15", "Incorrect procedure", "coding_error"),
    ("16", "Incorrect provider information", "administrative"),
    ("17", "Prior authorization required", "lack_of_preauth"),
    ("18", "Prior authorization obtained but incorrect", "lack_of_preauth"),
    ("19", "Early/late service date", "timely_filing"),
    ("20", "Lack of information", "missing_info"),
    ("21", "Non-covered service", "non_covered_service"),
    ("22", "Patient responsible (not secondary)", "patient_responsibility"),
    ("23", "Inpatient hospital service", "patient_responsibility"),
    ("24", "Provider/supplier suspension", "administrative"),
    ("25", "Provider/supplier exclusion", "administrative"),
    ("26", "Provider/supplier revocation", "administrative"),
    ("27", "Medically unnecessary", "medical_necessity"),
    ("28", "Patient not eligible", "patient_responsibility"),
    ("29", "Non-covered procedure", "non_covered_service"),
    ("30", "Coding error", "coding_error"),
    ("44", "Timely filing exceeded", "timely_filing"),
    ("50", "Not billed by required entity", "administrative"),
    ("51", "Incorrect claim type", "administrative"),
    ("52", "Not primary to another plan", "administrative"),
    ("53", "Medicare secondary payer", "administrative"),
    ("55", "Service not ordered by physician", "administrative"),
    ("65", "Prior service/claim resubmitted with wrong date", "timely_filing"),
    ("66", "Not submitted with required documents", "missing_info"),
    ("67", "Claim/encounter number invalid", "administrative"),
    ("68", "Place of service invalid", "administrative"),
    ("69", "Modifier missing/incorrect", "coding_error"),
    ("70", "Diagnosis pointer invalid", "coding_error"),
    ("71", "Non-covered charge", "non_covered_service"),
    ("72", "Incorrect payer ID", "administrative"),
    ("73", "Incorrect patient ID", "administrative"),
    ("74", "Incorrect claim ID", "administrative"),
    ("75", "Incorrect provider ID", "administrative"),
    ("76", "Incorrect service date", "administrative"),
    ("77", "Incorrect diagnosis code", "coding_error"),
    ("78", "Incorrect procedure code", "coding_error"),
    ("79", "Incorrect modifier", "coding_error"),
    ("80", "Incorrect units billed", "coding_error"),
    ("81", "Incorrect quantity", "coding_error"),
    ("82", "Incorrect payment amount", "administrative"),
    ("83", "Incorrect adjustment amount", "administrative"),
    ("84", "Incorrect charge amount", "administrative"),
    ("85", "Incorrect tax ID", "administrative"),
    ("86", "Incorrect group number", "administrative"),
    ("87", "Incorrect policy number", "administrative"),
    ("88", "Incorrect subscriber ID", "administrative"),
    ("89", "Incorrect dependent ID", "administrative"),
    ("90", "Incorrect relation code", "administrative"),
    ("91", "Incorrect sex", "administrative"),
    ("92", "Incorrect date of birth", "administrative"),
    ("93", "Incorrect name", "administrative"),
    ("94", "Incorrect address", "administrative"),
    ("95", "Incorrect phone number", "administrative"),
    ("96", "Incorrect ZIP code", "administrative"),
    ("97", "Incorrect state", "administrative"),
    ("98", "Incorrect country", "administrative"),
    ("99", "Other", "other"),
]

# Seed RARC codes
RARC_CODES = [
    ("1", "Claim/service lacks information or has provision/plan/carrier limitation"),
    ("2", "Missing encounter/claim number"),
    ("3", "Missing/invalid provider name"),
    ("4", "Missing/invalid provider ID"),
    ("5", "Missing/invalid date of birth"),
    ("6", "Missing/invalid sex"),
    ("7", "Missing/invalid relation code"),
    ("8", "Missing/invalid group number"),
    ("9", "Missing/invalid policy number"),
    ("10", "Missing/invalid subscriber ID"),
    ("11", "Missing/invalid dependent ID"),
    ("12", "Missing/invalid name"),
    ("13", "Missing/invalid address"),
    ("14", "Missing/invalid phone number"),
    ("15", "Missing/invalid ZIP code"),
    ("16", "Missing/invalid state"),
    ("17", "Missing/invalid country"),
    ("18", "Missing/invalid service date"),
    ("19", "Missing/invalid diagnosis code"),
    ("20", "Missing/invalid procedure code"),
    ("21", "Missing/invalid modifier"),
    ("22", "Missing/invalid units billed"),
    ("23", "Missing/invalid quantity"),
    ("24", "Missing/invalid payment amount"),
    ("25", "Missing/invalid adjustment amount"),
    ("26", "Missing/invalid charge amount"),
    ("27", "Missing/invalid tax ID"),
    ("28", "Missing/invalid referral number"),
    ("29", "Missing/invalid authorization number"),
    ("30", "Missing/invalid attestation number"),
    ("31", "Missing/invalid attachment number"),
    ("32", "Missing/invalid claim type"),
    ("33", "Missing/invalid place of service"),
    ("34", "Missing/invalid employment status"),
    ("35", "Missing/invalid patient status"),
    ("36", "Missing/invalid admission type"),
    ("37", "Missing/invalid diagnosis pointer"),
    ("38", "Missing/invalid rendering provider"),
    ("39", "Missing/invalid billing provider"),
    ("40", "Missing/invalid payee provider"),
    ("41", "Missing/invalid referring provider"),
    ("42", "Missing/invalid ordering provider"),
    ("43", "Missing/invalid supervising provider"),
    ("44", "Missing/invalid facility type"),
    ("45", "Missing/invalid facility type code"),
    ("46", "Missing/invalid claim frequency"),
    ("47", "Missing/invalid claim frequency code"),
    ("48", "Missing/invalid claim status"),
    ("49", "Missing/invalid claim status code"),
    ("50", "Missing/invalid claim type code"),
]


async def seed():
    conn = await asyncpg.connect(DATABASE_URL)
    try:
        print("Connecting to database...")

        # Check if admin user already exists
        existing = await conn.fetchval(
            "SELECT id FROM users WHERE username = $1", DEFAULT_USERNAME
        )
        if not existing:
            hashed = bcrypt.hashpw(
                DEFAULT_PASSWORD.encode("utf-8"), bcrypt.gensalt()
            ).decode("utf-8")
            await conn.execute(
                """INSERT INTO users (username, email, password_hash, full_name, role)
                   VALUES ($1, $2, $3, $4, $5)""",
                DEFAULT_USERNAME, DEFAULT_EMAIL, hashed, "System Administrator", DEFAULT_ROLE,
            )
            print(f"✓ Created default admin user (username: {DEFAULT_USERNAME}, password: {DEFAULT_PASSWORD})")
            print("  ⚠️  Change this password immediately after first login!")
        else:
            print(f"✓ Admin user '{DEFAULT_USERNAME}' already exists, skipping")

        # Seed CARC codes
        existing_carc = await conn.fetchval("SELECT COUNT(*) FROM carc_codes")
        if not existing_carc:
            # Each entry is (code, description, category) - the third element
            # is the carc_codes.category column. Unpacking only two names
            # raised ValueError on any database where these had not already
            # been loaded by database/seed/carc_codes.sql.
            for code, description, category in CARC_CODES:
                await conn.execute(
                    """INSERT INTO carc_codes (code, description, category)
                       VALUES ($1, $2, $3) ON CONFLICT (code) DO NOTHING""",
                    code, description, category,
                )
            print(f"✓ Seeded {len(CARC_CODES)} CARC codes")
        else:
            print(f"✓ CARC codes already exist ({existing_carc} rows), skipping")

        # Seed RARC codes
        existing_rarc = await conn.fetchval("SELECT COUNT(*) FROM rarc_codes")
        if not existing_rarc:
            for code, description in RARC_CODES:
                await conn.execute(
                    "INSERT INTO rarc_codes (code, description) VALUES ($1, $2) ON CONFLICT (code) DO NOTHING",
                    code, description,
                )
            print(f"✓ Seeded {len(RARC_CODES)} RARC codes")
        else:
            print(f"✓ RARC codes already exist ({existing_rarc} rows), skipping")

    finally:
        await conn.close()


if __name__ == "__main__":
    asyncio.run(seed())
