export type ModelComponent = {
  id: string;
  kind: string;
  filename: string;
  bytes: number;
  sha256: string;
};

/** Every runnable set has one of each. */
export const REQUIRED_KINDS = ['backbone', 'vae'] as const;
/** SheetSage2: without it the studio generates but cannot transcribe covers. */
export const OPTIONAL_KINDS = ['transcriber'] as const;
export const COMPONENT_KINDS = [...REQUIRED_KINDS, ...OPTIONAL_KINDS] as const;

const labels: Record<(typeof COMPONENT_KINDS)[number], string> = {
  backbone: 'Backbone',
  vae: 'VAE',
  transcriber: 'SheetSage2',
};

export const componentKindLabel = (kind: string) => labels[kind as keyof typeof labels] || kind;

export const isOptionalKind = (kind: string) => (OPTIONAL_KINDS as readonly string[]).includes(kind);

export const componentPrecision = (component: ModelComponent) => {
  const matched = component.filename.match(/-(BF16|F32|Q\d+(?:_K(?:_[MS])?|_0)?)\.gguf$/i);
  return matched?.[1]?.toUpperCase() || component.id;
};

export const componentsByKind = (components: ModelComponent[]) =>
  COMPONENT_KINDS.map((kind) => ({ kind, optional: isOptionalKind(kind), components: components.filter((component) => component.kind === kind) }));

/** The ids of a runnable set, or null while a required role is still unchosen. */
export const completeCustomComponentIds = (components: ModelComponent[], selectedByKind: Record<string, string>) => {
  const ids: string[] = [];
  for (const kind of COMPONENT_KINDS) {
    const id = selectedByKind[kind];
    if (!id) {
      if (isOptionalKind(kind)) continue;
      return null;
    }
    const component = components.find((entry) => entry.id === id);
    if (!component || component.kind !== kind) return null;
    ids.push(id);
  }
  return ids;
};

export const selectedComponentBytes = (components: ModelComponent[], ids: string[] | null) =>
  ids?.reduce((total, id) => total + (components.find((component) => component.id === id)?.bytes || 0), 0) || 0;
