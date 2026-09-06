# TLS certificates

Leave this directory empty and the frontend container generates a **self-signed**
certificate on first start (`tls.crt` / `tls.key`). That is encrypted but not
authenticated: browsers will warn, and they are right to.

To use a real certificate, drop it here as `tls.crt` and `tls.key` and restart
the frontend:

```bash
cp your-cert.pem  certs/tls.crt
cp your-key.pem   certs/tls.key
docker compose restart frontend
```

Set `TLS_MODE=provided` in `.env` so a missing or misnamed file fails loudly
instead of silently falling back to a self-signed certificate nobody notices.

`tls.crt` should contain the server certificate followed by any intermediates.

Nothing in here is committed — see `.gitignore`.
