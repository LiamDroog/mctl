"""Second stage after gen_isslive_items.py: give every item a display `section`, `row`, `col` and the
units evidence supports, so the telemetry pages can show matrices (e.g. CMG 1-4 by metric) instead of
a flat list. Idempotent: it rewrites the item blocks of config/isslive.toml in place."""
import re

P = "config/isslive.toml"
text = open(P).read()
head = text[: text.index("[[item]]")]
tail = text[text.index("# A unit-norm sanity check") :]
items = [dict(re.findall(r'(\w+) = "(.*)"', b)) for b in re.findall(r"\[\[item\]\]\n(.*?)(?=\n\n|\Z)", text, re.S)]

# Units: the description's own parenthesised unit wins over the spreadsheet's UNITS column, which is
# wrong for several items (checked against live values, docs/attitude-verification.md).
PAREN = {"kg/hr": "kg/h", "kPa": "kPa", "deg C": "degC", "N-m": "N·m", "Nms": "Nms", "g": "g", "amps": "A", "rpm": "rpm", "degrees": "deg", "deg": "deg", "degr": "deg", "kg": "kg"}
SHEET = {"CNT": "", "ND": "", "N/A": "", "N/S": "", "INTEGR": "", "DEG": "deg", "PCT": "%", "RPM": "rpm", "AMP": "A", "G": "g", "LBM/D": "lbm/d", "KG": "kg"}


def units(i):
    for g in reversed(re.findall(r"\(([^)]*)\)?", i["description"])):
        if g.strip() in PAREN:
            return PAREN[g.strip()]
    d = i["description"]
    if re.search(r"Drive Voltage", d):
        return "V"
    if re.search(r"Drive Current", d):
        return "A"
    if re.search(r"Fluid Temp", d):
        return "degC"  # spreadsheet says CNT; values 4-17 are Celsius-like (inferred, not published)
    return {**SHEET, "mmHg": "mmHg", "degC": "degC", "deg": "deg", "deg/s": "deg/s"}.get(i["units"], i["units"])


LABELS = [
    (r"S-Band RFG \(RFG (\d)\) Azimuth Gimbal Position", r"RFG \1 azimuth"),
    (r"S-Band RFG \(RFG (\d)\) Elevation Gimbal Position", r"RFG \1 elevation"),
    (r"S-Band RFG \(RFG (\d)\), on-off status", r"RFG \1 on/off"),
    (r"Ku-Band Video Downlink Channel (\d) Activity", r"Video downlink \1 activity"),
    (r"Video Source Routed to Downlink (\d)", r"Video source, downlink \1"),
    (r"Internal Audio Controller \(IAC\) - IAC-(\d) Active/Backup Indication", r"IAC-\1 active/backup"),
    (r"Space-To-Space Radio \(UHF\) (\d) Power", r"UHF radio \1 power"),
    (r"Space-To-Space Radio Frame Sync Lock", "UHF frame sync lock"),
    (r"Ku-Band SGANT Elevation Position", "Ku antenna elevation"),
    (r"Ku-Band SGANT Cross-Elevation Position", "Ku antenna cross-elevation"),
    (r"Ku-Band Transmit", "Ku transmit"),
    (r"Active String of S-Band", "Active S-band string"),
    (r"(Node [23]|Lab Port|Lab Starboard) Air Conditioner State", r"\1 air conditioner"),
    (r"Attitude Maneuver In Progress status", "Maneuver in progress"),
    (r"Attitude Source", "Attitude source"),
]


def short(label):
    for pat, rep in LABELS:
        label = re.sub(pat, rep, label)
    return label


def assign(i):
    """(section, row, col, label) for an item."""
    d, g, n = i["description"], i["group"], i["id"]
    m = re.search(r"CMG\)?-?\s?(\d)", d)
    cmg = f"CMG {m.group(1)}" if m else None
    if g == "attitude":
        q = re.search(r"Quaternion Component (\d)", d)
        if q and "Current" in d:
            return "Attitude quaternion (LVLH, scalar first)", f"q{q.group(1)}", "Current", f"Current q{q.group(1)}"
        if q:
            return "Attitude quaternion (LVLH, scalar first)", f"q{q.group(1)}", "Commanded", f"Commanded q{q.group(1)}"
        ax = re.search(r"(Roll|Pitch|Yaw) Error", d)
        if ax:
            return "Attitude error (current - commanded)", ax.group(1), "Error", f"{ax.group(1)} error"
        r = re.search(r"Rate ([XYZ])", d)
        if r:
            return "Inertial attitude rate", f"{r.group(1)} axis", "Rate", f"Rate {r.group(1)}"
        return "Attitude status (codes as published)", i["label"], "", i["label"]
    if g == "cmg":
        if cmg:
            for pat, row, sec in [("On-Line", "On-line", "CMG status"), ("Spin Motor Spin Bearing", "Spin bearing temp", "CMG temperatures"), ("Hall Resolver", "Hall resolver temp", "CMG temperatures"), ("Vibration", "Vibration", "CMG health"), ("Spin Motor Current", "Spin motor current", "CMG health"), ("Wheel Speed", "Wheel speed", "CMG health")]:
                if pat in d:
                    return sec, row, cmg, f"{row} {cmg}"
        lab = {"USLAB000005": "CMGs online", "USLAB000006": "Control torque, roll", "USLAB000007": "Control torque, pitch", "USLAB000008": "Control torque, yaw", "USLAB000009": "Momentum (active CMGs)", "USLAB000038": "Momentum capacity", "USLAB000011": "Desaturation request", "USLAB000041": "CMG control lost (alarm)", "USLAB000042": "ISS control lost (alarm)"}.get(n, i["label"])
        return "CMG momentum and control", lab, "", lab
    if g == "eps":
        sarj = re.search(r"SARJ\)? (Starboard|Port)", d)
        if sarj:
            return "SARJ angle", sarj.group(1), "Angle", f"SARJ {sarj.group(1).lower()}"
        a = re.search(r"Solar Array - (\d[AB])", d)
        if a:
            row = "BGA position" if "Beta Gimble" in d else "Drive voltage" if "Voltage" in d else "Drive current"
            return "Solar arrays (PVCU, names as published)", row, a.group(1), f"{row} {a.group(1)}"
    if g == "tcs":
        lp = re.match(r"Loop ([AB]) (Pump Flowrate|PM Out Press|PM Out Temp)", d)
        if lp:
            row = {"Pump Flowrate": "Pump flow", "PM Out Press": "Pump module out pressure", "PM Out Temp": "Pump module out temp"}[lp.group(2)]
            return "External thermal loops", row, f"Loop {lp.group(1)}", f"{row} loop {lp.group(1)}"
        mod = "Lab" if n.startswith("USLAB") else "Node 2" if n.startswith("NODE2") else "Node 3"
        for pat, row in [("Avionics Cooling", "Avionics fluid temp"), ("Air Cooling", "Air fluid temp")]:
            if pat in d:
                return "Internal cooling", row, mod, f"{row} {mod}"
        if "Coolant water" in d:
            suffix = " (LT)" if "LT" in d else " (MT)" if "MT" in d else ""
            return "Coolant water quantity", "Quantity" + suffix, mod, f"Coolant water {mod}{suffix}"
        return "Air conditioners", i["label"], "", i["label"]
    if g == "eclss":
        mod = "Lab" if n.startswith("USLAB") else "Node 3" if n.startswith("NODE3") else "Airlock"
        for pat, row in [("ppO2", "ppO2"), ("ppN2", "ppN2"), ("ppCO2", "ppCO2"), ("Cabin pressure", "Total pressure"), ("Airlock Pressure", "Total pressure"), ("Cabin temperature", "Temperature")]:
            if pat in d:
                return "Atmosphere", row, mod, f"{row} {mod}"
        if "Oxygen Generator" in d or "O2 Production" in d:
            return "Oxygen generation", i["label"], "", i["label"]
        return "Water and waste (Node 3)", i["label"], "", i["label"]
    if g == "comm":
        if "S-Band" in d or "S-Band" in i["label"] or n == "USLAB000092":
            return "S-band", i["label"], "", i["label"]
        if "Ku" in d or "Video" in d:
            return "Ku-band and video", i["label"], "", i["label"]
        return "UHF and audio", i["label"], "", i["label"]
    return g, i["label"], "", i["label"]


order = {}
esc = lambda s: s.replace("\\", "\\\\").replace('"', '\\"')
body = ""
seen = set()
for i in items:
    sec, row, col, label = assign(i)
    key = (i["group"], sec, row, col)
    if col and key in seen:  # the spreadsheet repeats some array names; keep both, visibly
        col += " #2"
    seen.add(key)
    i.update(section=sec, row=short(row), col=col, label=short(label), units=units(i))
    body += f'[[item]]\nid = "{i["id"]}"\ngroup = "{i["group"]}"\ndescription = "{esc(i["description"])}"\nlabel = "{esc(i["label"])}"\nsection = "{esc(sec)}"\nrow = "{esc(row)}"\ncol = "{esc(col)}"\nunits = "{esc(i["units"])}"\n\n'
if "`section`, `row`, `col`" not in head:
    head = head.rstrip() + "\n\n# `section`/`row`/`col` lay the items out on the pages (a matrix when `col` is set). `units` follow the\n# description's own unit where it gives one; the spreadsheet's UNITS column is wrong for several items.\n# Inferred (not published): fluid temperatures are degC. Evidence: docs/attitude-verification.md.\n\n"
open(P, "w").write(head + body + tail)
print(len(items), "items laid out")
