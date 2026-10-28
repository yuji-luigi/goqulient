// Entry points for the sidebar (VSX-11): the table/collection viewer and the ER diagram webviews.
export { openTable, tableTitle, type TableOptions } from './tablePanel.ts';
export { openErDiagram, erTitle, type ErOptions } from './erPanel.ts';
export type { Engine } from './panel.ts';
export type { ObjectRef, SchemaModel } from './protocol.ts';
