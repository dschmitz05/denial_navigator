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

EXPOSE 8000

# --workers 1 is deliberate. The dropzone FileWatcher is a background
# singleton started in the FastAPI startup event, so every extra uvicorn
# worker is a SECOND poller over the same directory - both see a new file,
# both parse it, both store it. With --workers 2 every dropzone file was
# ingested twice. If HTTP throughput ever matters here, move the watcher out
# into its own process rather than raising this number.
CMD ["uvicorn", "main:app", "--host", "0.0.0.0", "--port", "8000", "--workers", "1"]
