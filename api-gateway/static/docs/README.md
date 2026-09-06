# Vendored API documentation assets

Swagger UI and ReDoc are served from this directory rather than a CDN, so the
API docs work on an air-gapped host. FastAPI's built-in doc pages fetch these
from `cdn.jsdelivr.net` (and ReDoc additionally pulls Montserrat and Roboto
from Google Fonts); with no route off the box, every one of those requests
fails and the page renders blank white with nothing explaining why.

`api-gateway/main.py` serves `/docs` and `/redoc` by hand and points them here.

| File | Version | Source |
|------|---------|--------|
| `swagger-ui-bundle.js` | swagger-ui-dist 5.18.2 | `https://cdn.jsdelivr.net/npm/swagger-ui-dist@5.18.2/swagger-ui-bundle.js` |
| `swagger-ui.css` | swagger-ui-dist 5.18.2 | `https://cdn.jsdelivr.net/npm/swagger-ui-dist@5.18.2/swagger-ui.css` |
| `redoc.standalone.js` | redoc 2.1.5 | `https://cdn.jsdelivr.net/npm/redoc@2.1.5/bundles/redoc.standalone.js` |
| `favicon.svg` | — | written by hand; SVG so there is no binary to keep in sync |

## Upgrading

Re-download from a machine with network access and commit the result:

```bash
cd api-gateway/static/docs
curl -fLO https://cdn.jsdelivr.net/npm/swagger-ui-dist@<version>/swagger-ui-bundle.js
curl -fLO https://cdn.jsdelivr.net/npm/swagger-ui-dist@<version>/swagger-ui.css
curl -fL  https://cdn.jsdelivr.net/npm/redoc@<version>/bundles/redoc.standalone.js -o redoc.standalone.js
```

Pin an exact version, never a dist-tag. `redoc@next` is what broke here: the
tag was removed upstream and started returning 404, taking the docs page with
it.

## Verifying

The point of this directory is that the pages reference nothing off-host:

```bash
curl -s localhost:8000/docs  | grep -o 'https\?://[^"]*'   # must print nothing
curl -s localhost:8000/redoc | grep -o 'https\?://[^"]*'   # must print nothing
```
