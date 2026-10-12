import type { SemanticOptions, SemanticSession } from './semantic.js';
export type * from './semantic.js';
export { SemanticError } from './semantic.js';
export function createSemanticSession(options: SemanticOptions): Promise<SemanticSession>;
