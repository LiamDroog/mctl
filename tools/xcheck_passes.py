# Independent pass prediction: python-sgp4 + simple GMST rotation + own topocentric geometry.
import sys, json, math
sys.path.insert(0, sys.argv[1] + "/pysgp4")
from sgp4.api import Satrec, WGS72
from sgp4 import omm
import datetime as dt

rec = json.load(open(sys.argv[2]))[0]
sat = Satrec()
omm.initialize(sat, rec)
LAT, LON, ALT = 51.05, -114.07, 1045.0
a, f = 6378.137, 1/298.257223563
e2 = f*(2-f)
def site_ecef():
    la, lo = math.radians(LAT), math.radians(LON)
    n = a/math.sqrt(1-e2*math.sin(la)**2); h = ALT/1000
    return [(n+h)*math.cos(la)*math.cos(lo), (n+h)*math.cos(la)*math.sin(lo), (n*(1-e2)+h)*math.sin(la)]
S = site_ecef()
def gmst(jd):  # IAU-82, Vallado
    t = (jd - 2451545.0)/36525.0
    g = 67310.54841 + (876600*3600+8640184.812866)*t + 0.093104*t*t - 6.2e-6*t**3
    return math.radians((g % 86400)/240.0)
def look(unix):
    d = dt.datetime.fromtimestamp(unix, dt.UTC)
    jd, fr = (lambda y: (0,0))(0) if False else (None, None)
    from sgp4.api import jday
    jd, fr = jday(d.year, d.month, d.day, d.hour, d.minute, d.second + d.microsecond/1e6)
    err, r, v = sat.sgp4(jd, fr)
    if err: return None
    th = gmst(jd + fr)
    c, s = math.cos(th), math.sin(th)
    ecef = [c*r[0]+s*r[1], -s*r[0]+c*r[1], r[2]]
    dx = [ecef[i]-S[i] for i in range(3)]
    la, lo = math.radians(LAT), math.radians(LON)
    e = -math.sin(lo)*dx[0]+math.cos(lo)*dx[1]
    n = -math.sin(la)*math.cos(lo)*dx[0]-math.sin(la)*math.sin(lo)*dx[1]+math.cos(la)*dx[2]
    u = math.cos(la)*math.cos(lo)*dx[0]+math.cos(la)*math.sin(lo)*dx[1]+math.sin(la)*dx[2]
    return math.degrees(math.asin(u/math.sqrt(e*e+n*n+u*u)))
epoch = dt.datetime.fromisoformat(rec["EPOCH"]).replace(tzinfo=dt.UTC).timestamp()
mask = 10.0
def bis(lo, hi):
    up = look(lo) >= mask
    while hi-lo > 0.005:
        m = (lo+hi)/2
        if (look(m) >= mask) == up: lo = m
        else: hi = m
    return (lo+hi)/2
t, prev, aos, out = epoch, look(epoch) >= mask, None, []
while t < epoch + 7*86400:
    n = t + 5
    cur = look(n) >= mask
    if cur != prev:
        c = bis(t, n)
        if cur: aos = c
        elif aos is not None:
            ts = [aos + (c-aos)*k/400 for k in range(401)]
            tca = max(ts, key=look)
            out.append((aos, tca, c, look(tca)))
    prev, t = cur, n
for o in out: print("PASS %.3f %.3f %.3f %.3f" % o)
