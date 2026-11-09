"""Local REST API and status page for calibrated Raman spectra."""
import os
import socket
import threading
from pathlib import Path
from typing import Annotated, Literal

from fastapi import FastAPI, HTTPException, Request
from fastapi.responses import FileResponse, JSONResponse
from pydantic import BaseModel, Field

from spectra import Library, peaks

HERE = Path(__file__).parent
HOSTNAME = socket.gethostname().split(".", 1)[0]
library = Library(os.environ.get("SU3_INDEX", "/var/lib/su3-raman/index"))
app = FastAPI(title="SU(3) Raman", version="1.0", docs_url=None, redoc_url=None)
search_lock = threading.Lock()
SpectrumArray = Annotated[list[float], Field(min_length=8, max_length=65_536)]


class Spectrum(BaseModel):
    wavenumber: SpectrumArray
    intensity: SpectrumArray


class PeakRequest(Spectrum):
    min_prominence: float = Field(default=.03, gt=0, le=1)
    limit: int = Field(default=30, ge=1, le=100)


class SearchRequest(Spectrum):
    kind: Literal["measured", "computed", "both"] = "measured"
    sources: list[str] | None = Field(default=None, max_length=8)
    limit: int = Field(default=10, ge=1, le=30)


@app.middleware("http")
async def limit_requests(request: Request, call_next):
    if request.method == "POST":
        length = request.headers.get("content-length", "")
        if not length.isdecimal() or not 1 <= int(length) <= 3_000_000:
            return JSONResponse({"detail": "Request must contain up to 3 MB of JSON."}, status_code=413)
        if request.headers.get("content-type", "").split(";")[0] != "application/json":
            return JSONResponse({"detail": "Use application/json."}, status_code=415)
        origin = request.headers.get("origin")
        host = request.headers.get("host", "")
        if origin and (origin != f"http://{host}" or not (
            host.startswith(f"{HOSTNAME}.local:") or host.startswith("localhost:") or all(c.isdigit() or c in ".:" for c in host)
        )):
            return JSONResponse({"detail": "Use the local Raman site."}, status_code=403)
    response = await call_next(request)
    response.headers["X-Content-Type-Options"] = "nosniff"
    response.headers["Content-Security-Policy"] = (
        "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; "
        "base-uri 'none'; frame-ancestors 'none'"
    )
    return response


@app.get("/", include_in_schema=False)
def page():
    return FileResponse(HERE / "index.html")


@app.get("/site.css", include_in_schema=False)
def stylesheet():
    return FileResponse(HERE / "site.css")


@app.get("/site.js", include_in_schema=False)
def script():
    return FileResponse(HERE / "site.js", media_type="application/javascript")


@app.get("/api/status")
def status():
    return {"device": "SU(3) Raman", "mode": "offline spectral analysis",
            "hardware_acquisition": False, "sources": library.sources()}


@app.post("/api/peaks")
def detect(request: PeakRequest):
    try:
        return {"peaks": peaks(**request.model_dump())}
    except ValueError as exc:
        raise HTTPException(422, str(exc)) from exc


@app.post("/api/search")
def search(request: SearchRequest):
    if not search_lock.acquire(blocking=False):
        raise HTTPException(429, "A library search is already running.")
    try:
        return library.search(**request.model_dump())
    except ValueError as exc:
        raise HTTPException(422, str(exc)) from exc
    finally:
        search_lock.release()
