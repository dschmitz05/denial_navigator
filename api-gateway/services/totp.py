"""API Gateway — TOTP (RFC 6238) two-factor authentication.

Design decisions worth stating, because they are the ones that go wrong:

* **Secrets are encrypted at rest.** A TOTP secret is a password equivalent:
  anyone holding it can mint valid codes forever. Storing it in plaintext
  means a database dump defeats the second factor entirely, which is most of
  the reason for having one. Encrypted with a key derived from the deployment's
  own secret, so there is no new key to distribute.

* **An administrator never sees the secret.** They can require 2FA and they can
  reset it, but enrolment happens between the user and their device. That way
  an admin cannot impersonate a user, and does not become a place secrets leak
  from.

* **Codes are single-use.** A code is valid for a 30-second step, and without
  recording the step it satisfied, anyone who observes it - shoulder-surfing,
  a proxy log, a screenshot - can replay it inside that window.

* **One step of clock drift is allowed** in each direction. Phones drift; the
  alternative is users who cannot sign in and blame the application.
"""

import base64
import hashlib
import io
import logging
import os
import time
from typing import Optional

import pyotp
import qrcode
import qrcode.image.svg
from cryptography.fernet import Fernet, InvalidToken

logger = logging.getLogger("api_gateway.totp")

ISSUER = os.environ.get("TOTP_ISSUER", "Denial Navigator")

# One step either side of now. Wider than this starts to matter: each extra
# step is another 30 seconds in which an observed code stays usable.
VALID_WINDOW = 1
STEP_SECONDS = 30


def _fernet() -> Fernet:
    """Encryption key for stored secrets.

    Derived from TOTP_ENCRYPTION_KEY when set, otherwise from JWT_SECRET, so a
    deployment that has already been configured needs nothing new. Rotating
    either one makes existing enrolments undecryptable - which fails closed:
    users re-enrol, they do not silently lose their second factor.
    """
    material = os.environ.get("TOTP_ENCRYPTION_KEY") or os.environ.get("JWT_SECRET", "")
    if not material:
        raise RuntimeError("TOTP requires TOTP_ENCRYPTION_KEY or JWT_SECRET to be set")
    # Fernet wants 32 url-safe base64 bytes; the input is an arbitrary string.
    digest = hashlib.sha256(f"totp:{material}".encode()).digest()
    return Fernet(base64.urlsafe_b64encode(digest))


def new_secret() -> str:
    """A fresh base32 secret, as the authenticator app expects it."""
    return pyotp.random_base32()


def encrypt_secret(secret: str) -> str:
    return _fernet().encrypt(secret.encode()).decode()


def decrypt_secret(stored: str) -> Optional[str]:
    """Return the secret, or None if it cannot be decrypted.

    None means the encryption key changed. The caller treats that as 'not
    enrolled' rather than 'authenticated', so the failure mode is re-enrolment.
    """
    try:
        return _fernet().decrypt(stored.encode()).decode()
    except (InvalidToken, ValueError, TypeError):
        logger.error("stored TOTP secret could not be decrypted; key may have changed")
        return None


def provisioning_uri(secret: str, username: str) -> str:
    """The otpauth:// URI an authenticator app scans."""
    return pyotp.TOTP(secret).provisioning_uri(name=username, issuer_name=ISSUER)


def qr_svg(uri: str) -> str:
    """The URI as an inline SVG.

    Rendered here rather than by a JavaScript library so the enrolment screen
    works on an air-gapped deployment, and so the secret is never handed to a
    third-party script.
    """
    img = qrcode.make(uri, image_factory=qrcode.image.svg.SvgPathImage, box_size=10, border=2)
    buf = io.BytesIO()
    img.save(buf)
    return buf.getvalue().decode()


def current_step(at: Optional[float] = None) -> int:
    return int((at if at is not None else time.time()) // STEP_SECONDS)


def verify(secret: str, code: str, last_used_step: Optional[int]) -> tuple[bool, Optional[int]]:
    """Check a code. Returns (accepted, step_it_matched).

    The step is returned so the caller can persist it and refuse the same code
    a second time.
    """
    code = (code or "").strip().replace(" ", "")
    if not code.isdigit() or len(code) != 6:
        return False, None

    totp = pyotp.TOTP(secret)
    now = time.time()
    for offset in range(-VALID_WINDOW, VALID_WINDOW + 1):
        step = current_step(now) + offset
        if totp.verify(code, for_time=step * STEP_SECONDS, valid_window=0):
            if last_used_step is not None and step <= last_used_step:
                # Already spent. Refusing replay is the point of tracking it.
                logger.warning("TOTP code replayed (step %s already used)", step)
                return False, None
            return True, step
    return False, None
