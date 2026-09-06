FROM python:3.12-slim

WORKDIR /app

# Install system dependencies
RUN apt-get update && apt-get install -y --no-install-recommends \
    curl && \
    rm -rf /var/lib/apt/lists/*

# Copy requirements and install Python dependencies
COPY requirements.txt .
RUN pip install --no-cache-dir -r requirements.txt

# Copy application code
COPY . .

# Create dropzone and output directories
RUN mkdir -p /app/dropzone /app/output

# Run as an unprivileged user. A remote-code bug in a parser should not
# start life as root inside the container.
#
# The source files are mode 600 in the repository and COPY preserves that,
# so without the chmod the app user cannot read its own code.
RUN adduser --system --group --no-create-home app \
 && chown -R app:app /app \
 && chmod -R u=rwX,go=rX /app
RUN mkdir -p /app/dropzone /app/output && chown -R app:app /app/dropzone /app/output
USER app

EXPOSE 8000

# --workers 1 is deliberate. The dropzone FileWatcher is a background
# singleton started in the FastAPI startup event, so every extra uvicorn
# worker is a SECOND poller over the same directory - both see a new file,
# both parse it, both store it. With --workers 2 every dropzone file was
# ingested twice. If HTTP throughput ever matters here, move the watcher out
# into its own process rather than raising this number.
CMD ["uvicorn", "main:app", "--host", "0.0.0.0", "--port", "8000", "--workers", "1"]
