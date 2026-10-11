import json,sys
for c in sys.argv[1:]:
    r=json.load(open(f"runs/{c}/results.json"))
    print(c, r.get("fatal","")[:300])
    for o in r.get("ops",[]): print("  ",o["op"],o["status"],o.get("error",""), o.get("workaround",""))
