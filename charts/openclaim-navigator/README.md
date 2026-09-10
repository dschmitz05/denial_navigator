# OpenClaim Navigator Helm chart

This chart deploys the application services only. Provision PostgreSQL first,
initialize it with the repository's `database/init.sql` and migrations, then
create a Kubernetes Secret containing runtime credentials:

```bash
kubectl -n openclaim create secret generic openclaim-runtime \
  --from-literal=DATABASE_URL='postgres://…' \
  --from-literal=JWT_SECRET='…' \
  --from-literal=EDIPARSER_SERVICE_API_KEY='…' \
  --from-literal=LLM_SERVICE_API_KEY='…' \
  --from-literal=EDIPARSER_INTERNAL_API_KEY='…' \
  --from-literal=RAG_INTERNAL_API_KEY='…' \
  --from-literal=LLM_INTERNAL_API_KEY='…' \
  --from-literal=TOTP_FERNET_KEY='…'
helm upgrade --install openclaim charts/openclaim-navigator -n openclaim --create-namespace \
  --set ingress.host=claims.example.org --set ingress.tlsSecret=openclaim-tls
```

Publish the five service images to an approved registry and override
`images.*.repository`/`tag`. The chart intentionally leaves database, backup,
object-storage persistence, and TLS certificate lifecycle under the cluster
operator's existing controls. Configure persistent S3 object storage for
knowledge artifacts before production use; the default `emptyDir` is suitable
only for evaluation.
