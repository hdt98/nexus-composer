export interface IconMetadata {
  name: string; // Lowercase icon name, such as "openai".
  displayName: string; // Display name, such as "OpenAI".
  category: string; // Category, such as "ai-provider", "cloud", or "tool".
  keywords: string[]; // Search keywords.
  defaultColor?: string; // Default color.
}

export interface IconPreset {
  [key: string]: IconMetadata;
}
