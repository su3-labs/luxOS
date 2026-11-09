const form = document.querySelector("#spectrum");
const progress = document.querySelector("#progress");
const results = document.querySelector("#results");

function spectrum(text) {
    const wavenumber = [];
    const intensity = [];
    for (const [i, row] of text.trim().split(/\r?\n/).entries()) {
        const fields = row.trim().split(/[,;\t ]+/).map(s => s.replace(/^"|"$/g, ""));
        if (fields.length === 1 && !fields[0]) continue;
        const [x, y] = fields.map(Number);
        if (i === 0 && !Number.isFinite(x) && !Number.isFinite(y)) continue;
        if (fields.length !== 2 || fields.some(s => !s) || !Number.isFinite(x) || !Number.isFinite(y)) {
            throw new Error(`Use two numeric columns. Check row ${i + 1}.`);
        }
        wavenumber.push(x);
        intensity.push(y);
    }
    return { wavenumber, intensity };
}

function chart({wavenumber: x, intensity: y}) {
    if (x.length < 8) return;
    const xmin = Math.min(x[0], x.at(-1));
    const xmax = Math.max(x[0], x.at(-1));
    const ymin = y.reduce((a, v) => Math.min(a, v), Infinity);
    const ymax = y.reduce((a, v) => Math.max(a, v), -Infinity);
    const stride = Math.max(1, Math.floor(x.length / 2600));
    const points = [];
    for (let i = 0; i < x.length; i += stride) {
        points.push(`${(760 * (x[i] - xmin) / (xmax - xmin || 1)).toFixed(1)},${(195 - 190 * (y[i] - ymin) / (ymax - ymin || 1)).toFixed(1)}`);
    }
    const line = document.createElementNS("http://www.w3.org/2000/svg", "polyline");
    line.setAttribute("points", points.join(" "));
    document.querySelector("#chart").replaceChildren(line);
    document.querySelector("#axis").textContent = `${xmin.toFixed(1)}–${xmax.toFixed(1)} cm⁻¹`;
    document.querySelector("#figure").hidden = false;
}

function group(title, lines) {
    const heading = document.createElement("h2");
    heading.textContent = title;
    const list = document.createElement("ol");
    for (const text of lines.length ? lines : ["No results in this group."]) {
        const item = document.createElement("li");
        item.textContent = text;
        list.append(item);
    }
    results.append(heading, list);
}

form.addEventListener("submit", async event => {
    event.preventDefault();
    const action = event.submitter.value;
    const buttons = [...form.querySelectorAll("button")];
    buttons.forEach(b => b.disabled = true);
    results.replaceChildren();
    progress.textContent = action === "peaks" ? "Finding peaks…" : "Searching references…";
    try {
        const file = document.querySelector("#file").files[0];
        if (file.size > 3_000_000) throw new Error("Choose a spectrum smaller than 3 MB.");
        const data = spectrum(await file.text());
        chart(data);
        const kind = document.querySelector("#kind").value;
        if (action === "search") data.kind = kind;
        const response = await fetch(`/api/${action}`, {
            method: "POST", headers: {"Content-Type": "application/json"}, body: JSON.stringify(data)
        });
        const result = await response.json();
        if (!response.ok) {
            throw new Error(typeof result.detail === "string" ? result.detail : "Check the spectrum format and length.");
        }
        if (action === "peaks") {
            group("Detected peaks", result.peaks.map(p => `${p.shift_cm_1} cm⁻¹ · prominence ${p.relative_prominence}`));
            progress.textContent = "";
        } else {
            for (const name of kind === "both" ? ["measured", "computed"] : [kind]) {
                group(name === "measured" ? "Measured matches" : "Calculated matches",
                      result.matches[name].map(m => `${m.name || m.smiles || m.reference} · ${m.source} · ${m.score}`));
            }
            progress.textContent = result.interpretation;
        }
    } catch (error) {
        progress.textContent = error.message;
    } finally {
        buttons.forEach(b => b.disabled = false);
    }
});

fetch("/api/status").then(r => r.json()).then(status => {
    const list = document.querySelector("#sources");
    const values = Object.values(status.sources);
    for (const text of values.length ? values.map(s => `${s.title} · ${s.rows.toLocaleString()} references`)
                                     : ["Reference indexes are awaiting installation."]) {
        const item = document.createElement("li");
        item.textContent = text;
        list.append(item);
    }
}).catch(() => { document.querySelector("#sources").textContent = "Device status is unavailable."; });
