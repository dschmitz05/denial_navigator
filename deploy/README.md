# Production Compose deployment

This deployment combines the Rust base stack with the production override:

```bash
cp deploy/production.env.example deploy/production.env
chmod 600 deploy/production.env
mkdir -p certs
# Install a trusted certificate chain as certs/tls.crt and its private key as certs/tls.key.
docker compose --env-file deploy/production.env \
  -f docker-compose.rust.yml -f docker-compose.production.yml up -d --build
```

Only the frontend publishes ports: HTTP `80` redirects to HTTPS `443`; the API,
database, parser, and AI services remain on the private Compose network. The
override refuses placeholder or missing application secrets and requires a
provided TLS certificate. It also persists local knowledge-source artifacts in
the `artifacts` volume.

Before go-live, complete [the deployment security checklist](../docs/deployment-security-checklist.md), set a real external model endpoint or disable AI, and perform a backup/restore drill. The environment file and `certs/tls.key` contain secrets; keep both out of version control and restrict them to the service operator.

## Host-managed reverse proxy

If the host already manages certificates, use the reverse-proxy companion
override instead of mounting certificates into the frontend container:

```bash
docker compose --env-file deploy/production.env \
  -f docker-compose.rust.yml -f docker-compose.production.yml \
  -f docker-compose.reverse-proxy.yml up -d --build
```

This publishes the frontend only at `127.0.0.1:8080` and sets its
`TLS_MODE=off`; TLS terminates at the host proxy. Copy
[`nginx/openclaim.conf`](nginx/openclaim.conf) to the host, replace the sample
hostname/certificate paths, test with `nginx -t`, then reload nginx. Do not use
this override unless the proxy and frontend share the host or another trusted
private network.
