"""MCP tools for the SU(3) Raman node, served on SSH-forwarded loopback."""
from mcp.server.fastmcp import FastMCP

from api import PeakRequest, SearchRequest, detect, search, status
from typing import Any, Literal

server = FastMCP("SU(3) Raman", host="127.0.0.1", port=8765,
                 stateless_http=True, json_response=True)


@server.tool()
def raman_status() -> dict[str, Any]:
    """List the installed reference collections and supported device mode."""
    return status()


@server.tool()
def detect_peaks(wavenumber: list[float], intensity: list[float],
                 min_prominence: float = .03, limit: int = 30) -> dict[str, Any]:
    """Detect Raman peaks in a calibrated spectrum. Pass paired Raman shifts in cm⁻¹ and intensities."""
    return detect(PeakRequest(wavenumber=wavenumber, intensity=intensity,
                              min_prominence=min_prominence, limit=limit))


@server.tool()
def search_library(wavenumber: list[float], intensity: list[float],
                   kind: Literal["measured", "computed", "both"] = "measured",
                   sources: list[str] | None = None, limit: int = 10) -> dict[str, Any]:
    """Rank reference matches by similarity. Measured and calculated references are returned separately."""
    return search(SearchRequest(wavenumber=wavenumber, intensity=intensity,
                                kind=kind, sources=sources, limit=limit))


if __name__ == "__main__":
    server.run(transport="streamable-http")
