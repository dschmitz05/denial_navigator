#!/bin/sh
# Configure TLS before nginx starts.
#
# The application handles patient data and a login form; sending either over
# plain HTTP is a finding in any HIPAA review. It ships encrypted by default
# rather than leaving that to whoever deploys it, because "we will put a proxy
# in front of it" is the step that gets skipped.
#
# TLS_MODE:
#   self-signed  (default) generate a certificate if none is mounted, serve
#                HTTPS, redirect HTTP to it. Encrypted but NOT authenticated -
#                browsers will warn. Fine for a first run or an isolated site;
#                replace it before go-live.
#   provided     use the certificate mounted at /etc/nginx/certs. Refuses to
#                start without one, rather than silently falling back to a
#                self-signed cert nobody notices.
#   off          plain HTTP only. Valid ONLY when something else terminates
#                TLS over a trusted hop - a proxy on the same host or Docker
#                network. Across a VLAN this puts PHI on the wire in clear.
set -e

CERT_DIR=/etc/nginx/certs
CRT="$CERT_DIR/tls.crt"
KEY="$CERT_DIR/tls.key"
TLS_MODE="${TLS_MODE:-self-signed}"
TLS_HOSTNAME="${TLS_HOSTNAME:-localhost}"
PUBLIC_HTTPS_PORT="${PUBLIC_HTTPS_PORT:-3443}"
CONF=/etc/nginx/conf.d/default.conf

log() { echo "[tls] $*"; }

if [ "$TLS_MODE" = "off" ]; then
    log "TLS_MODE=off - serving plain HTTP. Terminate TLS in front of this container."
    cat > "$CONF" <<'EOF'
server {
    listen 80;
    server_name _;
    include /etc/nginx/snippets/app-locations.conf;
}
EOF
    exit 0
fi

if [ ! -f "$CRT" ] || [ ! -f "$KEY" ]; then
    if [ "$TLS_MODE" = "provided" ]; then
        log "ERROR: TLS_MODE=provided but no certificate at $CRT"
        log "Mount your certificate and key there, or set TLS_MODE=self-signed."
        exit 1
    fi
    log "No certificate found - generating a self-signed one for '$TLS_HOSTNAME'."
    log "This is encrypted but NOT authenticated: browsers will warn, and they"
    log "are right to. Replace it with a trusted certificate before go-live."
    mkdir -p "$CERT_DIR"
    openssl req -x509 -nodes -newkey rsa:2048 \
        -keyout "$KEY" -out "$CRT" -days 825 \
        -subj "/CN=$TLS_HOSTNAME/O=OpenClaim Navigator/OU=Self-signed" \
        -addext "subjectAltName=DNS:$TLS_HOSTNAME,DNS:localhost,IP:127.0.0.1" \
        >/dev/null 2>&1
    chmod 600 "$KEY"
    log "Generated $CRT (valid 825 days)."
else
    log "Using the certificate mounted at $CRT."
fi

# The HTTP listener only redirects. nginx cannot know which port the container
# is published on, so the target port is passed in; :443 is omitted because a
# URL with an explicit default port looks broken to users.
if [ "$PUBLIC_HTTPS_PORT" = "443" ]; then
    REDIRECT='return 301 https://$host$request_uri;'
else
    REDIRECT="return 301 https://\$host:$PUBLIC_HTTPS_PORT\$request_uri;"
fi

cat > "$CONF" <<EOF
server {
    listen 80;
    server_name _;
    $REDIRECT
}

server {
    listen 443 ssl;
    http2 on;
    server_name _;

    ssl_certificate     $CRT;
    ssl_certificate_key $KEY;

    # TLS 1.2 and 1.3 only. Anything older is a finding in its own right.
    ssl_protocols TLSv1.2 TLSv1.3;
    ssl_prefer_server_ciphers off;
    ssl_session_cache shared:SSL:10m;
    ssl_session_timeout 1d;
    ssl_session_tickets off;

    # Deliberately NO HSTS. With a self-signed certificate it would pin
    # browsers to a certificate they do not trust, and recovering from that
    # means clearing state on every client machine. Add it once a trusted
    # certificate is in place.

    include /etc/nginx/snippets/app-locations.conf;
}
EOF

log "TLS enabled (HTTPS on 443, HTTP redirects to :$PUBLIC_HTTPS_PORT)."
