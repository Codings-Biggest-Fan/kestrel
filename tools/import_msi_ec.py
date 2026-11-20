#!/usr/bin/env python3
"""Convert msi-ec's msi-ec.c register maps into Kestrel model TOML files.

Usage: import_msi_ec.py <msi-ec.c> <out_dir>
msi-ec is GPL-2.0-or-later: https://github.com/BeardOverflow/msi-ec
"""
import json, re, sys, pathlib

DECIMAL = {"bit", "max_mode", "max_state"}
SPECIAL = {"MSI_EC_ADDR_UNKNOWN": None, "MSI_EC_ADDR_UNSUPP": None}
MODE_NAMES = {"SM_TURBO_NAME": "turbo", "SM_ECO_NAME": "eco", "SM_COMFORT_NAME": "comfort",
              "SM_SPORT_NAME": "sport", "FM_AUTO_NAME": "auto", "FM_SILENT_NAME": "silent",
              "FM_BASIC_NAME": "basic", "FM_ADVANCED_NAME": "advanced"}

def val(tok):
    tok = tok.strip().rstrip(",")
    if tok in SPECIAL: return None
    if tok in ("true", "false"): return tok == "true"
    try: return int(tok, 0)
    except ValueError: return tok

def parse_block(body):
    out, i = {}, 0
    pat = re.compile(r"\.(\w+)\s*=\s*")
    while True:
        m = pat.search(body, i)
        if not m: break
        key, j = m.group(1), m.end()
        if body[j] == "{":
            depth, k = 0, j
            while True:
                if body[k] == "{": depth += 1
                elif body[k] == "}":
                    depth -= 1
                    if depth == 0: break
                k += 1
            inner = body[j + 1:k]
            if key == "modes":
                out[key] = {MODE_NAMES.get(n, n): int(v, 0) for n, v in re.findall(r"\{\s*(\w+)\s*,\s*(0x[0-9a-fA-F]+|\d+)\s*\}", inner)}
            elif "." in inner:
                out[key] = parse_block(inner)
            else:
                out[key] = [val(t) for t in inner.split(",") if t.strip()]
            i = k + 1
        else:
            k = body.find(",", j)
            nl = body.find("\n", j)
            if k == -1 or (nl != -1 and nl < k): k = nl
            out[key] = val(body[j:k])
            i = k + 1
    return out

def main(src, out_dir):
    text = pathlib.Path(src).read_text()
    text = re.sub(r"/\*.*?\*/", "", text, flags=re.S)
    fw = {}
    for name, body in re.findall(r"ALLOWED_FW_(\w+)\[\][^=]*=\s*\{(.*?)\};", text, re.S):
        fw[name] = [(f, (c or "").strip()) for f, c in re.findall(r'"([^"]+)"\s*,?\s*(?://\s*([^\n]*))?', body)]
    out = pathlib.Path(out_dir); out.mkdir(parents=True, exist_ok=True)
    index = []
    for name, body in re.findall(r"static struct msi_ec_conf CONF_(\w+)[^=]*=\s*\{(.*?)\n\};", text, re.S):
        conf = parse_block(body); conf.pop("allowed_fw", None)
        # Sections whose register is unsupported carry no usable data.
        conf = {k: v for k, v in conf.items()
                if not (isinstance(v, dict) and ("address" in v and v["address"] is None
                        or all(x is None for x in v.values() if not isinstance(x, (dict, list)))))}
        gen = "wmi2" if name.startswith("G2") else "wmi1"
        firmwares = fw.get(name, [])
        lines = [f"# Generated from msi-ec CONF_{name} (GPL-2.0-or-later). Do not edit by hand.",
                 f'id = "{name}"', f'wmi = "{gen}"', "firmware = ["]
        lines += [f'  {{ prefix = "{f}", models = {json.dumps(c)} }},' for f, c in firmwares]
        lines.append("]\n")
        def emit(prefix, d):
            scalars = {k: v for k, v in d.items() if not isinstance(v, dict)}
            if scalars or prefix:
                if prefix: lines.append(f"[{prefix}]")
                for k, v in scalars.items():
                    if v is None: continue
                    if isinstance(v, bool): lines.append(f"{k} = {str(v).lower()}")
                    elif isinstance(v, int): lines.append(f"{k} = {v}" if k in DECIMAL else f"{k} = 0x{v:02x}")
                    elif isinstance(v, list): lines.append(f"{k} = [{', '.join('0x%02x' % x for x in v if isinstance(x, int))}]")
                    else: lines.append(f'{k} = "{v}"')
                lines.append("")
            for k, v in d.items():
                if isinstance(v, dict):
                    if k == "modes":
                        lines.append(f"[{prefix}.modes]")
                        lines.extend([f"{m} = 0x{x:02x}" for m, x in v.items()] + [""])
                    else:
                        emit(f"{prefix}.{k}" if prefix else k, v)
        top = {k: v for k, v in conf.items() if not isinstance(v, dict)}
        for k, v in top.items():
            if v is not None: lines.append(f"{k} = 0x{v:02x}" if isinstance(v, int) else f"{k} = {json.dumps(v)}")
        lines.append("")
        for k, v in conf.items():
            if isinstance(v, dict): emit(k, v)
        (out / f"{name.lower()}.toml").write_text("\n".join(lines).rstrip() + "\n")
        index.append({"id": name, "wmi": gen, "firmware": [{"prefix": f, "models": c} for f, c in firmwares], "conf": conf})
    if len(sys.argv) > 3:
        pathlib.Path(sys.argv[3]).write_text(json.dumps(index, indent=1))
    print(f"{len(index)} configs, {sum(len(x['firmware']) for x in index)} firmware IDs")

if __name__ == "__main__":
    main(*sys.argv[1:3])
