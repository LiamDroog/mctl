"""One-off generator for config/isslive.toml's item list from ISS Mimic's public spreadsheet
(Telemetry/ISS_Public_Telemetry.xlsx, MIT). Usage: gen_isslive_items.py rows.json  (rows.json is the
sheet dumped as a list of {column: value}). Kept so the item list is reproducible, not hand-edited."""
import json, re, sys

rows = {}
for r in json.load(open(sys.argv[1]))[1:]:
    rows.setdefault(r["A"], r)


def rng(prefix, a, b):
    return [f"{prefix}{i:06d}" for i in range(a, b + 1)]


groups = {
    "cmg": rng("USLAB", 1, 9) + ["USLAB000011", "USLAB000038", "USLAB000041", "USLAB000042"] + rng("USLAB", 45, 52) + [f"Z1{i:06d}" for i in range(1, 13)],
    "eps": ["S0000003", "S0000004"] + [f"{p}{n:06d}" for p in ("P4", "P6", "S4", "S6") for n in (1, 2, 4, 5, 7, 8)],
    "tcs": [f"{p}{n:06d}" for p in ("P1", "S1") for n in (1, 2, 3)]
    + ["NODE2000001", "NODE2000002", "NODE2000003", "NODE2000006", "NODE2000007", "NODE3000012", "NODE3000013", "NODE3000017", "NODE3000018", "NODE3000019", "USLAB000056", "USLAB000057", "USLAB000060", "USLAB000061", "USLAB000064", "USLAB000065"],
    "eclss": ["USLAB000053", "USLAB000054", "USLAB000055", "USLAB000058", "USLAB000059"] + [f"NODE3{n:06d}" for n in range(1, 12)] + ["AIRLOCK000054"],
    "comm": ["P1000004", "P1000005", "P1000007", "S1000004", "S1000009"] + rng("USLAB", 88, 101) + ["Z1000013", "Z1000014", "Z1000015"],
}
rules = [("Control Moment Gyroscope (CMG)", "CMG"), ("Photovolatic Control Unit (PVCU) - Solar Array - ", "PVCU "), ("US Current Local Vertical Local Horizontal (LVLH) Attitude Quaternion Component ", "LVLH attitude quat q"), ("US Commanded Attitude Quaternion Component ", "Commanded attitude quat q"), ("Beta Gimble Assembly (BGA)", "BGA"), ("Solar Alpha Rotary Joint (SARJ)", "SARJ"), ("Radio Frequency Group", "RFG"), ("Multiplexer/Demultiplexer", "MDM"), ("US ", ""), ("Attitude Control Reference Frame", "Control reference frame")]


def label(d):
    for a, b in rules:
        d = d.replace(a, b)
    return d


old = open("config/isslive.toml").read()
head = old[: old.index("[[item]]")].rstrip()
tail = old[old.index("# A unit-norm sanity check") :]
items = [dict(re.findall(r'(\w+) = "(.*)"', b)) for b in re.findall(r"\[\[item\]\]\n(.*?)(?=\n\n|\Z)", old, re.S)]
seen = {i["id"] for i in items}
unit_fix = {**{i: "mmHg" for i in ["USLAB000053", "USLAB000054", "USLAB000055", "USLAB000058", "NODE3000001", "NODE3000002", "NODE3000003", "AIRLOCK000054"]}, "USLAB000059": "degC", "USLAB000022": "deg", "USLAB000023": "deg", "USLAB000024": "deg", "USLAB000025": "deg/s", "USLAB000026": "deg/s", "USLAB000027": "deg/s"}
out = []
for d in items:
    if d["id"] in ("S0000003", "S0000004", "P4000007"):
        d["group"] = "eps"
    d["units"] = unit_fix.get(d["id"], d["units"])
    d["label"] = label(d["description"])
    out.append(d)
missing = []
for g, ids in groups.items():
    for i in ids:
        if i in seen:
            continue
        r = rows.get(i)
        if not r:
            missing.append(i)
            continue
        seen.add(i)
        out.append({"id": i, "group": g, "description": r["C"], "units": r.get("F") or "N/A", "label": label(r["C"])})
assert not missing, missing
esc = lambda s: s.replace("\\", "\\\\").replace('"', '\\"')
body = "".join(f'[[item]]\nid = "{d["id"]}"\ngroup = "{d["group"]}"\ndescription = "{esc(d["description"])}"\nlabel = "{esc(d["label"])}"\nunits = "{esc(d["units"])}"\n\n' for d in out)
head += "\n\n# `label` is a short display name derived from `description` by abbreviation only. `units` is the\n# spreadsheet's UNITS column, EXCEPT USLAB000022-27, which it labels RAD and RAD/S but whose live values\n# are deg and deg/s (docs/attitude-verification.md).\n\n"
open("config/isslive.toml", "w").write(head + body + tail)
print(len(out), "items")
