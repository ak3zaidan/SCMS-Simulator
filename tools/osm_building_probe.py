"""Phase 2 de-risk: are building polygons already complete in the existing OSM cache?"""
import sys
import xml.etree.ElementTree as ET

path = sys.argv[1]
root = ET.parse(path).getroot()

nodes = {n.get("id"): (float(n.get("lat")), float(n.get("lon")))
         for n in root.findall("node")}
print("nodes in cache:", len(nodes))

buildings, incomplete, closed = [], 0, 0
for w in root.findall("way"):
    tags = {t.get("k"): t.get("v") for t in w.findall("tag")}
    if "building" not in tags:
        continue
    refs = [nd.get("ref") for nd in w.findall("nd")]
    coords = [nodes[r] for r in refs if r in nodes]
    if len(coords) != len(refs):
        incomplete += 1
        continue
    if len(coords) >= 4 and coords[0] == coords[-1]:
        closed += 1
    buildings.append((tags.get("building"), coords, tags.get("height") or tags.get("building:levels")))

print("building ways with FULL geometry: %d   incomplete: %d   closed rings: %d"
      % (len(buildings), incomplete, closed))
if buildings:
    kinds = {}
    withh = 0
    for k, c, h in buildings:
        kinds[k] = kinds.get(k, 0) + 1
        if h:
            withh += 1
    print("with height/levels tag: %d (%.0f%%)" % (withh, 100.0 * withh / len(buildings)))
    print("top building kinds:", sorted(kinds.items(), key=lambda z: -z[1])[:6])
    k, c, h = buildings[0]
    print("sample polygon: kind=%s pts=%d height=%s first=%s" % (k, len(c), h, c[0]))
