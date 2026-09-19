/**
 * The scenario form: a field list flattened out of a JSON Schema, with units and help text.
 *
 * The engine is asked for the real schema first — `scenario.get {with_schema: true}` (§6.10) returns
 * `scenario-1.json`, which 03-interfaces §13 says is "published with help text and units for every
 * field". When the engine does not supply one (the mock server accepts the parameter and returns no
 * `schema` key), the form falls back to {@link PHASE1_FIELDS}, a hand-written description of the
 * Phase 1 fields, and the panel says which of the two it is using.
 *
 * Values are addressed by RFC 6901 JSON Pointer, which is also what `scenario.set {patch}` takes, so
 * an edit in the form is one patch operation with no translation layer.
 */

/** A widget in the scenario form. */
export interface FormField {
  /** RFC 6901 JSON Pointer into the scenario document. */
  readonly pointer: string;
  readonly label: string;
  readonly group: string;
  readonly kind: "string" | "number" | "integer" | "boolean" | "enum" | "json";
  readonly unit?: string;
  readonly help?: string;
  readonly options?: readonly string[];
  readonly min?: number;
  readonly max?: number;
  readonly step?: number;
}

/** Read a JSON Pointer out of a document; `undefined` when any step is missing. */
export function getPointer(doc: unknown, pointer: string): unknown {
  if (pointer === "" || pointer === "/") return doc;
  let cur: unknown = doc;
  for (const rawPart of pointer.split("/").slice(1)) {
    const part = rawPart.replace(/~1/g, "/").replace(/~0/g, "~");
    if (cur === null || typeof cur !== "object") return undefined;
    cur = (cur as Record<string, unknown>)[part];
  }
  return cur;
}

/** Write a JSON Pointer into a copy of `doc`, creating intermediate objects. */
export function setPointer<T>(doc: T, pointer: string, value: unknown): T {
  if (pointer === "" || pointer === "/") return value as T;
  const parts = pointer.split("/").slice(1).map((p) => p.replace(/~1/g, "/").replace(/~0/g, "~"));
  const root: Record<string, unknown> = { ...(doc as unknown as Record<string, unknown>) };
  let cur = root;
  for (let i = 0; i < parts.length - 1; i++) {
    const key = parts[i];
    const next = cur[key];
    cur[key] = next !== null && typeof next === "object" && !Array.isArray(next)
      ? { ...(next as Record<string, unknown>) }
      : {};
    cur = cur[key] as Record<string, unknown>;
  }
  cur[parts[parts.length - 1]] = value;
  return root as unknown as T;
}

interface JsonSchemaNode {
  type?: string | string[];
  title?: string;
  description?: string;
  unit?: string;
  enum?: unknown[];
  minimum?: number;
  maximum?: number;
  multipleOf?: number;
  properties?: Record<string, JsonSchemaNode>;
  items?: JsonSchemaNode;
  [key: string]: unknown;
}

function kindOf(node: JsonSchemaNode): FormField["kind"] {
  if (Array.isArray(node.enum) && node.enum.length > 0) return "enum";
  const type = Array.isArray(node.type) ? node.type.find((t) => t !== "null") : node.type;
  switch (type) {
    case "integer": return "integer";
    case "number": return "number";
    case "boolean": return "boolean";
    case "string": return "string";
    default: return "json";
  }
}

/** Title-case a property name: `duration_s` → `Duration s`. */
function humanise(key: string): string {
  const spaced = key.replace(/[_-]+/g, " ").trim();
  return spaced.charAt(0).toUpperCase() + spaced.slice(1);
}

/**
 * Flatten an object-typed JSON Schema into a form. Objects one level down become groups; anything
 * deeper, and anything the form has no widget for, is offered as a JSON text area so no field of the
 * engine's schema silently disappears.
 */
export function fieldsFromJsonSchema(schema: unknown, maxFields = 120): FormField[] {
  const root = schema as JsonSchemaNode | null;
  if (!root || typeof root !== "object" || !root.properties) return [];
  const out: FormField[] = [];

  const walk = (node: JsonSchemaNode, pointer: string, group: string, depth: number): void => {
    if (out.length >= maxFields) return;
    const props = node.properties;
    if (!props) return;
    for (const [key, child] of Object.entries(props)) {
      if (out.length >= maxFields) return;
      const childPointer = `${pointer}/${key.replace(/~/g, "~0").replace(/\//g, "~1")}`;
      const type = Array.isArray(child.type) ? child.type.find((t) => t !== "null") : child.type;
      if (type === "object" && child.properties && depth < 2) {
        walk(child, childPointer, depth === 0 ? humanise(key) : `${group} · ${humanise(key)}`, depth + 1);
        continue;
      }
      out.push({
        pointer: childPointer,
        label: child.title ?? humanise(key),
        group: group === "" ? "General" : group,
        kind: kindOf(child),
        ...(typeof child.unit === "string" ? { unit: child.unit } : {}),
        ...(typeof child.description === "string" ? { help: child.description } : {}),
        ...(Array.isArray(child.enum) ? { options: child.enum.map(String) } : {}),
        ...(typeof child.minimum === "number" ? { min: child.minimum } : {}),
        ...(typeof child.maximum === "number" ? { max: child.maximum } : {}),
        ...(typeof child.multipleOf === "number" ? { step: child.multipleOf } : {}),
      });
    }
  };

  walk(root, "", "", 0);
  return out;
}

/**
 * The Phase 1 scenario fields (03-interfaces §13, restricted to what Phase 1 of 10-roadmap covers),
 * used when the engine does not publish `scenario-1.json` over `scenario.get {with_schema:true}`.
 * Pointers match the document the mock engine serves.
 */
export const PHASE1_FIELDS: readonly FormField[] = [
  { pointer: "/meta/name", label: "Name", group: "Meta", kind: "string", help: "Scenario name; appears in the run manifest and in Hello.str_scenario_name (§3.1.1)." },
  { pointer: "/meta/description", label: "Description", group: "Meta", kind: "string" },
  { pointer: "/seed", label: "Master seed", group: "Meta", kind: "integer", help: "Sub-stream seeds are derived from this; never set them individually (03-interfaces §13)." },

  { pointer: "/time/t0", label: "Wall-clock t₀", group: "Time", kind: "string", unit: "ISO 8601 UTC", help: "Maps simulated time 0 onto the wall clock; travels as Hello.t0_wall_ns (§3.1.1)." },
  { pointer: "/time/duration_s", label: "Duration", group: "Time", kind: "integer", unit: "s", min: 1, max: 86400, help: "Run length in simulated seconds; bounds the scrub bar." },
  { pointer: "/time/step_ms", label: "Mobility step", group: "Time", kind: "integer", unit: "ms", min: 10, max: 1000, step: 10, help: "Δt_mob — the delta cadence (§3.1.1 mobility_step_ns, default 100 ms)." },

  { pointer: "/world/source", label: "Source", group: "World", kind: "enum", options: ["synthetic", "osm", "sumo", "json", "editor"], help: "Where the world geometry comes from (03-interfaces §13 world.source.kind)." },
  { pointer: "/world/bbox", label: "Bounding box", group: "World", kind: "json", unit: "[min_lon, min_lat, max_lon, max_lat]", help: "WGS-84, the same array world.import_osm takes (§6.11)." },
  { pointer: "/world/buildings", label: "Buildings", group: "World", kind: "boolean", help: "Extrude building footprints; needed for NLOSb propagation and for the 3D view." },

  { pointer: "/traffic/actors", label: "Actors", group: "Traffic", kind: "integer", unit: "count", min: 1, max: 50000, help: "Concurrent actors; drives Hello.actor_capacity (§3.1.1) and the render budget (09-ui §4)." },
  { pointer: "/traffic/classes", label: "Classes", group: "Traffic", kind: "json", help: "Actor class names; must match the class table of §3.1.4." },

  { pointer: "/radio/tiers/phy", label: "PHY tier", group: "Radio", kind: "enum", options: ["abstract", "medium", "high"], help: "03-interfaces §13: phy 'high' requires mac 'high'." },
  { pointer: "/radio/tiers/mac", label: "MAC tier", group: "Radio", kind: "enum", options: ["abstract", "medium", "high"] },

  { pointer: "/security/profile", label: "Credential protocol", group: "Security", kind: "enum", options: ["scms", "etsi-ts102941", "none"], help: "Which credential-management protocol the nodes run (03-interfaces §7)." },
  { pointer: "/security/attackers/share", label: "Attacker share", group: "Security", kind: "number", unit: "fraction", min: 0, max: 1, step: 0.01, help: "Fraction of equipped nodes that are attackers; the GT ATTACKER bit of §3.3.4." },
];

/** A stable group order for the form, whichever source the fields came from. */
export function groupsOf(fields: readonly FormField[]): string[] {
  const seen: string[] = [];
  for (const f of fields) if (!seen.includes(f.group)) seen.push(f.group);
  return seen;
}
