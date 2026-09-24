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
 * The standard scenario fields, used when the engine does not publish its own list.
 *
 * The help text is what a researcher needs to set the field, not where the field is specified. It
 * used to cite a document section in half of these, which told the reader nothing they could act
 * on — and worse, it implied the value would not make sense without reading that document.
 *
 * Every pointer is checked against the document a real engine serves on `scenario.get` —
 * `scenarios/phase1-manhattan.yaml`, resolved. The list previously addressed `/traffic/actors`,
 * `/time/step_ms`, `/world/source` as a string, `/world/bbox`, `/security/profile` and
 * `/security/attackers/share`, none of which exist in that document, and it typed `/seed` as an
 * integer against a value of `"0x000000c0ffee5eed"`. So two thirds of the form was blank against a
 * scenario that configures every one of those things, and a blank box labelled "Attacker share"
 * reads as "this scenario has no attackers in it" when what it means is that the form was looking
 * in the wrong place. The seed — the one field that decides whether a result can be reproduced —
 * showed as empty.
 *
 * Labels stay in the user's language and the pointer is what travels, so nothing is lost by a
 * researcher not knowing that "fitted with radios" is spelt `equipped_fraction`.
 */
export const PHASE1_FIELDS: readonly FormField[] = [
  { pointer: "/meta/name", label: "Name", group: "Description", kind: "string", help: "What this scenario is called. It appears in the header and in the run's own record." },
  { pointer: "/meta/description", label: "Description", group: "Description", kind: "string" },
  {
    pointer: "/seed",
    label: "Master seed",
    group: "Description",
    kind: "string",
    help: "Two runs with the same seed and the same settings produce the same results. Every random choice in the run derives from this one number. Decimal, or hexadecimal with a 0x prefix.",
  },

  { pointer: "/time/t0", label: "Start date and time", group: "Time", kind: "string", unit: "UTC", help: "The real date and time that the start of the run stands for. It affects nothing in the simulation except what the clocks say." },
  { pointer: "/time/duration_s", label: "Duration", group: "Time", kind: "number", unit: "s", min: 1, max: 86400, help: "How long the run lasts in simulated time. This is the length of the timeline under the map." },
  { pointer: "/time/mobility_step_ms", label: "Mobility step", group: "Time", kind: "integer", unit: "ms", min: 10, max: 1000, step: 10, help: "How often every vehicle moves. Smaller is more accurate and more expensive; 100 ms is the usual choice." },

  { pointer: "/world/source/kind", label: "Streets from", group: "World", kind: "enum", options: ["osm-xml", "osm-pbf", "sumo", "procedural", "json"], help: "Where the streets come from: imported from OpenStreetMap, taken from a traffic simulation, or generated." },
  { pointer: "/world/source/path", label: "Map file", group: "World", kind: "string", help: "The file the streets are read from, when they are imported rather than generated." },
  { pointer: "/world/source/bbox", label: "Area to import", group: "World", kind: "json", unit: "degrees", help: "The corners of the area to take from the map file, in latitude and longitude." },
  { pointer: "/world/highway_preset", label: "Road rules", group: "World", kind: "string", help: "Which country's lane widths, speed limits and signal timings to assume wherever the map does not say." },
  { pointer: "/world/buildings/enabled", label: "Buildings", group: "World", kind: "boolean", help: "Draw buildings, and let them block radio signals. Without this every vehicle has line of sight to every other." },

  { pointer: "/actors/vehicles/demand/rate_veh_per_h", label: "Vehicles entering", group: "Traffic", kind: "number", unit: "per hour", min: 0, help: "How much traffic to inject. This is the main thing that decides how busy the network gets, and how heavy the run is to compute and to draw." },
  { pointer: "/actors/vehicles/demand/kind", label: "How they arrive", group: "Traffic", kind: "string", help: "The model that decides when each vehicle appears — for example a Poisson process at the rate above." },
  { pointer: "/actors/vehicles/equipped_fraction", label: "Fitted with radios", group: "Traffic", kind: "number", unit: "fraction", min: 0, max: 1, step: 0.01, help: "The share of vehicles carrying a communication unit. The rest are still traffic: they fill the roads and block signals, and send nothing." },
  { pointer: "/actors/vru/pedestrians", label: "Pedestrians", group: "Traffic", kind: "integer", min: 0, help: "Walking road users. Counted separately because they move differently and are the hardest to detect." },
  { pointer: "/actors/vru/cyclists", label: "Cyclists", group: "Traffic", kind: "integer", min: 0 },

  { pointer: "/radio/rat", label: "Radio technology", group: "Radio", kind: "enum", options: ["dsrc-80211p", "lte-v2x-pc5", "nr-v2x-pc5"], help: "Which radio the vehicles use to talk to one another: 802.11p (DSRC), or the LTE or 5G NR sidelink (PC5)." },
  { pointer: "/radio/tiers/phy", label: "Signal detail", group: "Radio", kind: "enum", options: ["abstract", "medium", "high"], help: "How carefully the radio signal itself is modelled. The most detailed setting also requires the most detailed channel-access model below." },
  { pointer: "/radio/tiers/mac", label: "Channel-access detail", group: "Radio", kind: "enum", options: ["abstract", "medium", "high"], help: "How carefully vehicles taking turns on a shared channel is modelled." },
  { pointer: "/radio/tiers/propagation", label: "Propagation detail", group: "Radio", kind: "enum", options: ["abstract", "medium", "high"], help: "How carefully the signal's path through the streets and around buildings is modelled." },

  { pointer: "/security/envelope", label: "Message security", group: "Security", kind: "enum", options: ["ieee1609.2", "etsi-ts103097", "none"], help: "Which standard's signed message format the vehicles use, which is what lets a receiver tell a genuine message from a forged one." },
  { pointer: "/security/signature", label: "Signature algorithm", group: "Security", kind: "enum", options: ["ecdsa-p256", "ecdsa-brainpool256", "ecdsa-p384", "sm2"] },
  { pointer: "/security/crypto_mode", label: "Cryptography", group: "Security", kind: "enum", options: ["modeled", "real"], help: "“Modeled” charges the time and the bytes a signature costs without doing the arithmetic; “real” actually signs and verifies, which is slower and exactly right." },
  { pointer: "/security/pseudonym_change/strategy", label: "Identity changes", group: "Security", kind: "enum", options: ["time", "distance", "none", "adaptive"], help: "How a vehicle decides when to switch to a fresh temporary identity. This is what stops it being followed from message to message." },
  { pointer: "/security/pseudonym_change/period_s", label: "…how often", group: "Security", kind: "number", unit: "s", min: 1, help: "With the “time” strategy, how long a vehicle keeps one identity before changing it." },
  { pointer: "/security/verification_policy", label: "Verification order", group: "Security", kind: "string", help: "Which arriving messages a receiver checks first when more arrive than it can check in time." },

  { pointer: "/messages/sets", label: "Messages sent", group: "Messages", kind: "json", help: "Which message types the vehicles broadcast — for example basic safety messages, ten times a second." },
  { pointer: "/metrics", label: "Measurements", group: "Messages", kind: "json", help: "Which figures to compute — for example packet delivery ratio. Each one appears in the plots along the bottom of this page." },

  { pointer: "/nodes/default_obu", label: "Hardware profile", group: "Hardware", kind: "string", help: "Which real on-board unit's timings, transmit power and queue sizes to use. Every number it affects carries a link back to its model card." },
  { pointer: "/nodes/compute_tier", label: "Compute detail", group: "Hardware", kind: "enum", options: ["abstract", "medium", "high"], help: "How carefully the time a unit takes to do its own work is modelled." },

  { pointer: "/weather/initial", label: "Weather", group: "Conditions", kind: "enum", options: ["clear", "rain", "snow", "fog"], help: "Rain and snow weaken the signal and change how people drive." },
  { pointer: "/weather/intensity", label: "…how heavy", group: "Conditions", kind: "number", unit: "fraction", min: 0, max: 1, step: 0.05 },
  { pointer: "/threats/attackers", label: "Attackers", group: "Conditions", kind: "json", help: "Misbehaving vehicles, and what each one does. Empty means none — an honest baseline to measure a detector against." },
  { pointer: "/threats/jammers", label: "Jammers", group: "Conditions", kind: "json", help: "Transmitters whose purpose is to stop everyone else being heard." },
];

/** A stable group order for the form, whichever source the fields came from. */
export function groupsOf(fields: readonly FormField[]): string[] {
  const seen: string[] = [];
  for (const f of fields) if (!seen.includes(f.group)) seen.push(f.group);
  return seen;
}

// ---------------------------------------------------------------------------------------------
// The engine's published settings surface
// ---------------------------------------------------------------------------------------------

/**
 * Whether the engine acts on a field, as `crates/v2xw-engine`'s `KEY_STATUS` table says.
 *
 * The page never decides this itself: it is read off the `x-status` the engine publishes for every
 * leaf, so a key someone wires changes what the form says the moment the engine is rebuilt.
 */
export type FieldStatus = "wired" | "partial" | "not-implemented" | "refused" | "descriptive" | "unknown";

/** A form field with the engine's own statement of whether it changes the run. */
export interface PublishedFormField extends FormField {
  readonly status: FieldStatus;
  /** The engine's sentence about what it does with the field. */
  readonly statusNote: string;
  /** True for a list or a map, edited as JSON because its elements have no fixed pointer. */
  readonly collection: boolean;
}

/** One row of the published field index, or one node of the published schema. */
export interface PublishedRow {
  readonly [key: string]: unknown;
}

function statusOf(value: unknown): FieldStatus {
  return value === "wired" || value === "partial" || value === "not-implemented" || value === "refused" || value === "descriptive"
    ? value
    : "unknown";
}

/** Every schema node that carries an `x-pointer`, by pointer — the first one wins. */
function schemaIndex(schema: unknown): Map<string, PublishedRow> {
  const out = new Map<string, PublishedRow>();
  const walk = (node: unknown): void => {
    if (Array.isArray(node)) {
      for (const child of node) walk(child);
      return;
    }
    if (node === null || typeof node !== "object") return;
    const row = node as PublishedRow;
    const pointer = row["x-pointer"];
    if (typeof pointer === "string" && !out.has(pointer)) out.set(pointer, row);
    for (const child of Object.values(row)) walk(child);
  };
  walk(schema);
  return out;
}

function kindOfRow(row: PublishedRow): FormField["kind"] {
  if (Array.isArray(row.enum) && row.enum.length > 0) return "enum";
  switch (row.kind) {
    case "integer": return "integer";
    case "number": return "number";
    case "boolean": return "boolean";
    case "enum": return "enum";
    case "string": return "string";
    default: return "json";
  }
}

/**
 * The settings form, built from the engine's published field index (`scenario.get {with_schema}`
 * `fields`) and its schema.
 *
 * Three kinds of row come out of it:
 *
 *  * **a leaf** at a fixed pointer, with the widget its type calls for;
 *  * **a collection** — a list or a map, whose element leaves the engine publishes under `/-/` or
 *    `/*` because an element has no fixed pointer — edited as one JSON value at the collection's
 *    own pointer, carrying the collection's status;
 *  * **a choice of variant** for a tagged union such as `world.source`, whose tag is not a leaf of
 *    any variant but is what picks between them.
 *
 * Every row carries the engine's `x-status`, which is the point: a field the engine does not act on
 * is marked in the form, not silently accepted.
 */
export function fieldsFromPublished(
  fields: readonly PublishedRow[],
  schema: unknown,
  doc: unknown,
): PublishedFormField[] {
  const index = schemaIndex(schema);
  const out: PublishedFormField[] = [];
  const seen = new Set<string>();
  const push = (field: PublishedFormField): void => {
    if (seen.has(field.pointer)) return;
    seen.add(field.pointer);
    out.push(field);
  };
  const fromRow = (
    row: PublishedRow,
    pointer: string,
    collection: boolean,
    kind: FormField["kind"],
  ): PublishedFormField => ({
    pointer,
    label: typeof row.title === "string" ? row.title : humanise(pointer.split("/").pop() ?? pointer),
    group: typeof row["x-group"] === "string" ? row["x-group"] : "Other",
    kind,
    ...(typeof row.unit === "string" && row.unit !== "" ? { unit: row.unit } : {}),
    ...(typeof row.description === "string" ? { help: row.description } : {}),
    ...(kind === "enum" && Array.isArray(row.enum) ? { options: row.enum.map(String) } : {}),
    ...(typeof row.minimum === "number" ? { min: row.minimum } : {}),
    ...(typeof row.exclusiveMinimum === "number" ? { min: row.exclusiveMinimum } : {}),
    ...(typeof row.maximum === "number" ? { max: row.maximum } : {}),
    status: statusOf(row["x-status"]),
    statusNote: typeof row["x-status-note"] === "string" ? row["x-status-note"] : "",
    collection,
  });

  for (const row of fields) {
    const pointer = row["x-pointer"];
    if (typeof pointer !== "string" || pointer === "") continue;
    const cut = pointer.search(/\/[-*](\/|$)/);
    if (cut >= 0) {
      const root = pointer.slice(0, cut);
      const rootRow = index.get(root) ?? row;
      push(fromRow({ ...rootRow, "x-group": rootRow["x-group"] ?? row["x-group"] }, root, true, "json"));
      continue;
    }
    // A tagged union's tag sits beside its variants' leaves; offer it where its first leaf is.
    for (const [unionPointer, node] of index) {
      if (typeof node["x-tag"] !== "string" || !Array.isArray(node["x-variants"])) continue;
      if (!pointer.startsWith(`${unionPointer}/`)) continue;
      const name = typeof node.title === "string" ? node.title : humanise(unionPointer.split("/").pop() ?? "");
      push({
        ...fromRow(node, `${unionPointer}/${node["x-tag"]}`, false, "enum"),
        label: `${name}: kind`,
        options: (node["x-variants"] as unknown[]).map(String),
      });
    }
    push(fromRow(row, pointer, false, kindOfRow(row)));
  }

  // A top-level section the index has no leaf for (a plain list of names, say) is still a setting.
  if (doc !== null && typeof doc === "object") {
    for (const key of Object.keys(doc as Record<string, unknown>)) {
      const pointer = `/${key}`;
      if (out.some((f) => f.pointer === pointer || f.pointer.startsWith(`${pointer}/`))) continue;
      push(fromRow(index.get(pointer) ?? {}, pointer, true, "json"));
    }
  }
  return out;
}

/** The pointers at which `a` and `b` differ, down to the leaves, sorted. */
export function changedPointers(a: unknown, b: unknown, at = ""): string[] {
  const isObject = (v: unknown): v is Record<string, unknown> =>
    v !== null && typeof v === "object" && !Array.isArray(v);
  if (isObject(a) && isObject(b)) {
    const keys = [...new Set([...Object.keys(a), ...Object.keys(b)])].sort();
    return keys.flatMap((k) => changedPointers(a[k], b[k], `${at}/${k.replace(/~/g, "~0").replace(/\//g, "~1")}`));
  }
  return JSON.stringify(a) === JSON.stringify(b) ? [] : [at === "" ? "/" : at];
}
