// Saved ER positions (workspaceState) and how they merge with a fresh `er/layout`. Pure, so the Node tests run it.
import type { LayoutRect, Point, SchemaModel, Size } from './protocol.ts';

/** Same cap as the app's `ERTabModel.load` (more boxes make the diagram unusable). */
export const MAX_ENTITIES = 1500;

/** Gap between the boxes placed by hand and a block of new ones laid out below them. */
const NEW_BLOCK_GAP = 80;

/** Reads saved positions tolerantly: anything that isn't `{id: {x, y}}` with finite numbers is skipped, never thrown. */
export function decodeSaved(value: unknown): Record<string, Point> {
  const out: Record<string, Point> = {};
  const positions = value && typeof value === 'object' && 'positions' in value ? (value as { positions: unknown }).positions : value;
  if (!positions || typeof positions !== 'object' || Array.isArray(positions)) return out;
  for (const [id, p] of Object.entries(positions as Record<string, unknown>)) {
    if (!p || typeof p !== 'object') continue;
    const { x, y } = p as { x?: unknown; y?: unknown };
    if (typeof x === 'number' && typeof y === 'number' && Number.isFinite(x) && Number.isFinite(y)) out[id] = { x, y };
  }
  return out;
}

/** The value written to workspaceState (versioned, so the format can change without losing positions). */
export function encodeSaved(positions: Record<string, Point>): { version: 1; positions: Record<string, Point> } {
  return { version: 1, positions };
}

/** Keeps at most `max` entities (and the relationships between them), with a warning like the app's. */
export function capEntities(model: SchemaModel, max = MAX_ENTITIES): SchemaModel {
  if (model.entities.length <= max) return model;
  const entities = model.entities.slice(0, max);
  const ids = new Set(entities.map((e) => e.id));
  return {
    ...model,
    entities,
    relationships: model.relationships.filter((r) => ids.has(r.fromEntity) && ids.has(r.toEntity)),
    warnings: [...model.warnings, `Showing the first ${max} of ${model.entities.length} tables.`],
  };
}

/** Child → parent edges for `er/layout`. */
export function layoutEdges(model: SchemaModel, only?: ReadonlySet<string>): { child: string; parent: string }[] {
  return model.relationships
    .filter((r) => !only || (only.has(r.fromEntity) && only.has(r.toEntity)))
    .map((r) => ({ child: r.fromEntity, parent: r.toEntity }));
}

/** Splits entity ids into those with a saved position and the new ones. */
export function partition(ids: readonly string[], saved: Record<string, Point>): { known: string[]; fresh: string[] } {
  const known: string[] = [];
  const fresh: string[] = [];
  for (const id of ids) (Object.hasOwn(saved, id) ? known : fresh).push(id);
  return { known, fresh };
}

/**
 * Saved positions win; new entities keep their relative fresh layout but move, as one block, below everything placed
 * by hand, so they never land on top of a saved box (the app overlays the full auto layout and can overlap).
 */
export function mergeLayout(
  saved: Record<string, Point>,
  sizes: Record<string, Size>,
  fresh: Record<string, LayoutRect>,
): Record<string, Point> {
  const out: Record<string, Point> = {};
  let minX = Infinity, maxY = -Infinity;
  for (const [id, p] of Object.entries(saved)) {
    const s = sizes[id];
    if (!s) continue; // a table that's gone (or outside these schemas)
    out[id] = p;
    minX = Math.min(minX, p.x);
    maxY = Math.max(maxY, p.y + s.height);
  }
  const added = Object.entries(fresh).filter(([id]) => !(id in out));
  if (!added.length) return out;
  if (minX === Infinity) {
    for (const [id, r] of added) out[id] = { x: r.origin.x, y: r.origin.y };
    return out;
  }
  const fx = Math.min(...added.map(([, r]) => r.origin.x));
  const fy = Math.min(...added.map(([, r]) => r.origin.y));
  for (const [id, r] of added) out[id] = { x: r.origin.x - fx + minX, y: r.origin.y - fy + maxY + NEW_BLOCK_GAP };
  return out;
}
