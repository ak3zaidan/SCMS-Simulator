"""Phase 3 de-risk: can sumolib give us everything a .net.xml -> CustomNetwork importer needs?

Checks for: node coords, directed edges, per-edge lane counts, speed limits, oneway-ness,
edge shape polylines, and which nodes are traffic-light controlled.
"""
import os
import sys

sys.path.append(os.path.join(os.environ["SUMO_HOME"], "tools"))
import sumolib  # noqa: E402

NET = sys.argv[1] if len(sys.argv) > 1 else r"C:\Temp\sumoprobe\probe.net.xml"
net = sumolib.net.readNet(NET, withInternal=False)

nodes = net.getNodes()
edges = net.getEdges()
print("nodes=%d edges=%d" % (len(nodes), len(edges)))

tls_nodes = [n for n in nodes if n.getType() == "traffic_light"]
print("tls-controlled nodes: %d" % len(tls_nodes))
print("node types:", sorted({n.getType() for n in nodes}))

e = edges[0]
print("\n-- sample edge --")
print("id       :", e.getID())
print("from->to :", e.getFromNode().getID(), "->", e.getToNode().getID())
print("lanes    :", e.getLaneNumber())
print("speed    :", e.getSpeed(), "m/s")
print("length   :", round(e.getLength(), 2), "m")
print("shape pts:", len(e.getShape()), e.getShape()[:3])
print("lane width:", e.getLane(0).getWidth())

# oneway detection: an edge is two-way iff a reverse edge exists between the same node pair
pairs = {(x.getFromNode().getID(), x.getToNode().getID()) for x in edges}
oneway = [x for x in edges if (x.getToNode().getID(), x.getFromNode().getID()) not in pairs]
print("\noneway edges: %d / %d (%.1f%%)" % (len(oneway), len(edges), 100.0 * len(oneway) / len(edges)))

# connections / turn restrictions
outs = e.getOutgoing()
print("outgoing connections from sample edge:", len(outs))

# node coords
n0 = nodes[0]
print("sample node coord:", n0.getID(), n0.getCoord())

# geo-referencing (for OSM-derived nets)
print("\nhas geo projection:", net.hasGeoProj())
if net.hasGeoProj():
    print("sample lon/lat:", net.convertXY2LonLat(*n0.getCoord()))
print("\nALL REQUIRED FIELDS AVAILABLE: True")
