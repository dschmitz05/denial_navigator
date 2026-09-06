FROM python:3.12-slim

WORKDIR /app

RUN apt-get update && apt-get install -y --no-install-recommends \
    curl && \
    rm -rf /var/lib/apt/lists/*

COPY requirements.txt .
RUN pip install --no-cache-dir -r requirements.txt

COPY . .

# Run as an unprivileged user. A remote-code bug in a parser should not
# start life as root inside the container.
#
# The source files are mode 600 in the repository and COPY preserves that,
# so without the chmod the app user cannot read its own code.
RUN adduser --system --group --no-create-home app \
 && chown -R app:app /app \
 && chmod -R u=rwX,go=rX /app
USER app

EXPOSE 8000

CMD ["uvicorn", "main:app", "--host", "0.0.0.0", "--port", "8000", "--workers", "2"]
