FROM python:3.11-slim-bookworm
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates e2fsprogs gdisk util-linux && rm -rf /var/lib/apt/lists/*
COPY requirements.txt /luxos-requirements.txt
RUN pip install --only-binary=:all: --no-cache-dir --target /luxos-deps -r /luxos-requirements.txt
