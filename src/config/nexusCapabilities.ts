export interface NexusCapabilities {
  readonly reasoningBoundary: "think_close";
}

export const NEXUS_CAPABILITIES = {
  reasoningBoundary: "think_close",
} as const satisfies NexusCapabilities;
