import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { TFunction } from "i18next";
import { useForm } from "react-hook-form";
import { Form } from "@/components/ui/form";
import type { ProviderCategory } from "@/types";
import {
  ProviderPresetSelector,
  filterPresetEntries,
  getPresetDisplayName,
  getPresetSearchText,
  getVisiblePresetEntries,
  sortPresetEntries,
  type PresetSortMode,
} from "@/components/providers/forms/ProviderPresetSelector";

// Mock ProviderIcon to avoid depending on the icon library's implementation.
vi.mock("@/components/ProviderIcon", () => ({
  ProviderIcon: ({
    icon,
    name,
    color,
    size,
  }: {
    icon?: string;
    name: string;
    color?: string;
    size?: number;
  }) => (
    <span
      data-testid="provider-icon"
      data-icon={icon}
      data-name={name}
      data-color={color}
      data-size={size}
    />
  ),
}));

const presetCategoryLabels = {
  official: "Official",
  cn_official: "Open-source Official",
  aggregator: "Aggregator",
  third_party: "Third Party",
};

const translations: Record<string, string> = {
  "preset.alpha": "Alpha Localized",
  "preset.gamma": "Gamma Localized",
};

const t = ((key: string) => translations[key] ?? key) as TFunction;

type TestPresetEntry = {
  id: string;
  preset: {
    name: string;
    nameKey?: string;
    websiteUrl: string;
    settingsConfig: Record<string, never>;
    category: ProviderCategory;
    primePartner?: boolean;
  };
};

const presetEntries: TestPresetEntry[] = [
  {
    id: "gamma",
    preset: {
      name: "Gamma Raw",
      nameKey: "preset.gamma",
      websiteUrl: "https://gamma.example.com",
      settingsConfig: {},
      category: "aggregator",
    },
  },
  {
    id: "alpha",
    preset: {
      name: "Alpha Raw",
      nameKey: "preset.alpha",
      websiteUrl: "https://alpha.example.com/v1",
      settingsConfig: {},
      category: "official",
    },
  },
  {
    id: "beta",
    preset: {
      name: "Beta Gateway",
      websiteUrl: "https://CN-Gateway.example.com",
      settingsConfig: {},
      category: "cn_official",
    },
  },
  {
    id: "delta",
    preset: {
      name: "Delta Mirror",
      websiteUrl: "https://delta.example.com",
      settingsConfig: {},
      category: "third_party",
    },
  },
] satisfies TestPresetEntry[];

function getIds(entries: ReadonlyArray<{ id: string }>) {
  return entries.map((entry) => entry.id);
}

function renderSelector({
  entries = presetEntries,
  onPresetChange = vi.fn(),
}: {
  entries?: TestPresetEntry[];
  onPresetChange?: (value: string) => void;
} = {}) {
  const Wrapper = () => {
    const form = useForm();

    return (
      <Form {...form}>
        <ProviderPresetSelector
          selectedPresetId="custom"
          presetEntries={entries}
          presetCategoryLabels={presetCategoryLabels}
          onPresetChange={onPresetChange}
        />
      </Form>
    );
  };

  return render(<Wrapper />);
}

function getPresetButtonTexts() {
  const knownNames = new Set([
    "providerPreset.custom",
    ...presetEntries.flatMap((entry) => [
      entry.preset.name,
      entry.preset.nameKey ?? entry.preset.name,
    ]),
  ]);

  return screen
    .getAllByRole("button")
    .map((button) => button.textContent?.trim() ?? "")
    .filter((text) => knownNames.has(text));
}

function getSearchButton() {
  return screen.getByRole("button", {
    name: /providerPreset\.(search|searchAriaLabel|openSearch)|search/i,
  });
}

function getSortButton() {
  return screen.getByRole("button", {
    name: /providerPreset\.(sort|sortByName|restoreOriginalOrder)|sort by name|restore original order|sort/i,
  });
}

function getSearchInput() {
  return screen.getByRole("textbox", {
    name: /providerPreset\.(searchInput|searchPlaceholder)|search presets|search/i,
  });
}

describe("ProviderPresetSelector pure helpers", () => {
  it("prefers the translated nameKey for display and falls back to the raw name", () => {
    expect(getPresetDisplayName(presetEntries[1].preset, t)).toBe(
      "Alpha Localized",
    );
    expect(getPresetDisplayName(presetEntries[2].preset, t)).toBe(
      "Beta Gateway",
    );
  });

  it("builds lowercase search text from display and raw names without URL or category labels", () => {
    const searchText = getPresetSearchText(presetEntries[1], t);

    expect(searchText).toContain("alpha localized");
    expect(searchText).toContain("alpha raw");
    expect(searchText).not.toContain("example.com");
    expect(searchText).not.toContain("official");
    expect(searchText).toBe(searchText.toLowerCase());
  });

  it("returns the original array for an empty query and matches non-empty queries case-insensitively", () => {
    expect(filterPresetEntries(presetEntries, "   ", t)).toBe(presetEntries);
    expect(
      getIds(filterPresetEntries(presetEntries, "ALPHA LOCALIZED", t)),
    ).toEqual(["alpha"]);
  });

  it("matches names only and does not search URLs or category labels", () => {
    expect(
      getIds(filterPresetEntries(presetEntries, "cn-gateway.example.com", t)),
    ).toEqual([]);
    expect(getIds(filterPresetEntries(presetEntries, "Aggregator", t))).toEqual(
      [],
    );
  });

  it("sorts A-Z, prioritizes official presets in original mode, and filters before sorting visible entries", () => {
    const originalMode: PresetSortMode = "original";
    const nameAscMode: PresetSortMode = "nameAsc";

    const original = sortPresetEntries(presetEntries, originalMode, t);
    expect(original).not.toBe(presetEntries);
    // Original mode prioritizes the official preset (alpha) and preserves the input order of the rest.
    expect(getIds(original)).toEqual(["alpha", "gamma", "beta", "delta"]);

    expect(getIds(sortPresetEntries(presetEntries, nameAscMode, t))).toEqual([
      "alpha",
      "beta",
      "delta",
      "gamma",
    ]);
    expect(getIds(presetEntries)).toEqual(["gamma", "alpha", "beta", "delta"]);

    expect(
      getIds(
        getVisiblePresetEntries(presetEntries, {
          query: "a",
          sortMode: nameAscMode,
          t,
        }),
      ),
    ).toEqual(["alpha", "beta", "delta", "gamma"]);
  });

  it("uses stable official, prime partner, and remaining groups in original mode without duplicates", () => {
    // Deliberately scramble the input order to verify that:
    // - official presets come first in encounter order (officialOnly, officialPrime);
    // - non-official prime partners come next (primeOnly);
    // - remaining presets preserve their input order (restFirst, restLast);
    // - a preset that is both official and a prime partner appears only in the official group.
    const mixed: TestPresetEntry[] = [
      {
        id: "restFirst",
        preset: {
          name: "Rest First",
          websiteUrl: "https://rest-first.example.com",
          settingsConfig: {},
          category: "third_party",
        },
      },
      {
        id: "primeOnly",
        preset: {
          name: "Prime Only",
          websiteUrl: "https://prime-only.example.com",
          settingsConfig: {},
          category: "cn_official",
          primePartner: true,
        },
      },
      {
        id: "officialOnly",
        preset: {
          name: "Official Only",
          websiteUrl: "https://official-only.example.com",
          settingsConfig: {},
          category: "official",
        },
      },
      {
        id: "officialPrime",
        preset: {
          name: "Official Prime",
          websiteUrl: "https://official-prime.example.com",
          settingsConfig: {},
          category: "official",
          primePartner: true,
        },
      },
      {
        id: "restLast",
        preset: {
          name: "Rest Last",
          websiteUrl: "https://rest-last.example.com",
          settingsConfig: {},
          category: "aggregator",
        },
      },
    ];

    expect(getIds(sortPresetEntries(mixed, "original", t))).toEqual([
      "officialOnly",
      "officialPrime",
      "primeOnly",
      "restFirst",
      "restLast",
    ]);
  });
});

describe("ProviderPresetSelector", () => {
  it("prioritizes official presets by default and preserves the input order of the rest", () => {
    renderSelector();

    expect(getPresetButtonTexts()).toEqual([
      "providerPreset.custom",
      "preset.alpha",
      "preset.gamma",
      "Beta Gateway",
      "Delta Mirror",
    ]);
  });

  it("sorts standard presets A-Z and restores original order on the next sort click", async () => {
    const user = userEvent.setup();
    renderSelector();

    await user.click(getSortButton());

    expect(getPresetButtonTexts()).toEqual([
      "providerPreset.custom",
      "Beta Gateway",
      "Delta Mirror",
      "preset.alpha",
      "preset.gamma",
    ]);

    await user.click(getSortButton());

    expect(getPresetButtonTexts()).toEqual([
      "providerPreset.custom",
      "preset.alpha",
      "preset.gamma",
      "Beta Gateway",
      "Delta Mirror",
    ]);
  });

  it("filters only standard presets and always retains the custom configuration", async () => {
    const user = userEvent.setup();
    renderSelector();

    await user.click(getSearchButton());
    await user.type(getSearchInput(), "gateway");

    expect(
      screen.getByRole("button", { name: "providerPreset.custom" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Beta Gateway" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "preset.gamma" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "preset.alpha" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Delta Mirror" }),
    ).not.toBeInTheDocument();
  });

  it("retains the custom configuration and shows an empty state when no standard preset matches", async () => {
    const user = userEvent.setup();
    renderSelector();

    await user.click(getSearchButton());
    await user.type(getSearchInput(), "not-found");

    expect(
      screen.getByRole("button", { name: "providerPreset.custom" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "preset.gamma" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "preset.alpha" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Beta Gateway" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Delta Mirror" }),
    ).not.toBeInTheDocument();
    expect(
      screen.getByText(
        /providerPreset\.(empty|noResults)|no matching presets|no results/i,
      ),
    ).toBeInTheDocument();
  });

  it("uses full-width preset buttons for equal grid-column alignment", () => {
    renderSelector();

    const presetButtons = screen.getAllByRole("button");
    const fullWidthButtons = presetButtons.filter((btn) =>
      btn.className.includes("w-full"),
    );

    // At least custom plus four presets are full-width; search and sort controls are size-8.
    expect(fullWidthButtons.length).toBeGreaterThanOrEqual(5);
  });

  it("renders an icon element when preset.icon is present", () => {
    const entriesWithIcon = [
      {
        id: "with-icon",
        preset: {
          name: "With Icon",
          websiteUrl: "https://icon.example.com",
          settingsConfig: {},
          category: "official" as ProviderCategory,
          icon: "claude-api",
          iconColor: "#D4915D",
        },
      },
    ];

    renderSelector({ entries: entriesWithIcon });

    const button = screen.getByRole("button", { name: /with icon/i });
    const icon = button.querySelector('[data-testid="provider-icon"]');
    expect(icon).not.toBeNull();
    expect(icon?.getAttribute("data-icon")).toBe("claude-api");
    expect(icon?.getAttribute("data-color")).toBe("#D4915D");
  });

  it("renders a placeholder for text alignment when neither preset.icon nor theme.icon exists", () => {
    const entriesWithoutIcon = [
      {
        id: "no-icon",
        preset: {
          name: "No Icon",
          websiteUrl: "https://noicon.example.com",
          settingsConfig: {},
          category: "official" as ProviderCategory,
        },
      },
    ];

    renderSelector({ entries: entriesWithoutIcon });

    const button = screen.getByRole("button", { name: /no icon/i });
    // A 16x16 placeholder keeps the label aligned with buttons that have icons.
    const placeholder = button.querySelector("span[aria-hidden]");
    expect(placeholder).not.toBeNull();
  });

  it("renders the same alignment placeholder in the custom button", () => {
    renderSelector();

    const customButton = screen.getByRole("button", {
      name: "providerPreset.custom",
    });
    const placeholder = customButton.querySelector("span[aria-hidden]");
    expect(placeholder).not.toBeNull();
  });

  it("toggles the inline search input and closes and clears it with Escape", async () => {
    const user = userEvent.setup();
    renderSelector();

    // The search input is initially closed.
    expect(
      screen.queryByRole("textbox", {
        name: /providerPreset\.(searchInput|searchPlaceholder)|search presets|search/i,
      }),
    ).not.toBeInTheDocument();

    // Clicking the search control opens the input.
    await user.click(getSearchButton());
    const input = getSearchInput();
    expect(input).toBeInTheDocument();

    // Entering a keyword filters the presets.
    await user.type(input, "gateway");
    expect(
      screen.getByRole("button", { name: "Beta Gateway" }),
    ).toBeInTheDocument();

    // Escape closes and clears the input.
    await user.keyboard("{Escape}");
    expect(
      screen.queryByRole("textbox", {
        name: /providerPreset\.(searchInput|searchPlaceholder)|search presets|search/i,
      }),
    ).not.toBeInTheDocument();
    // Closing the input restores all presets.
    expect(
      screen.getByRole("button", { name: "preset.gamma" }),
    ).toBeInTheDocument();
  });

  it("opens the search input with the Ctrl+F shortcut", async () => {
    const user = userEvent.setup();
    renderSelector();

    // The search input is initially closed.
    expect(
      screen.queryByRole("textbox", {
        name: /providerPreset\.(searchInput|searchPlaceholder)|search presets|search/i,
      }),
    ).not.toBeInTheDocument();

    // Ctrl+F opens the input.
    await user.keyboard("{Control>}f{/Control}");
    expect(getSearchInput()).toBeInTheDocument();
  });

  it("selects a preset after searching without clearing the query", async () => {
    const user = userEvent.setup();
    const onPresetChange = vi.fn();
    renderSelector({ onPresetChange });

    await user.click(getSearchButton());
    await user.type(getSearchInput(), "gateway");

    await user.click(screen.getByRole("button", { name: "Beta Gateway" }));

    expect(onPresetChange).toHaveBeenCalledWith("beta");
    // The search input stays open and retains its query.
    expect(getSearchInput()).toBeInTheDocument();
    expect(getSearchInput()).toHaveValue("gateway");
  });

  it("refocuses an open search input with Ctrl+F without clearing its query", async () => {
    const user = userEvent.setup();
    renderSelector();

    await user.click(getSearchButton());
    await user.type(getSearchInput(), "gateway");

    // Selecting a preset moves focus away while leaving the search input open.
    await user.click(screen.getByRole("button", { name: "Beta Gateway" }));
    expect(getSearchInput()).not.toHaveFocus();

    // Calling setSearchOpen(true) again does not rerender or retrigger autoFocus,
    // so the shortcut must imperatively restore focus without clearing the query.
    await user.keyboard("{Control>}f{/Control}");
    await waitFor(() => expect(getSearchInput()).toHaveFocus());
    expect(getSearchInput()).toHaveValue("gateway");
  });

  it("closes and clears the search input when clicking outside the component", async () => {
    const user = userEvent.setup();
    const Wrapper = () => {
      const form = useForm();
      return (
        <Form {...form}>
          <ProviderPresetSelector
            selectedPresetId="custom"
            presetEntries={presetEntries}
            presetCategoryLabels={presetCategoryLabels}
            onPresetChange={vi.fn()}
          />
          <div data-testid="outside">Outside</div>
        </Form>
      );
    };
    render(<Wrapper />);

    await user.click(getSearchButton());
    await user.type(getSearchInput(), "gateway");
    expect(getSearchInput()).toBeInTheDocument();

    // Clicking outside the component closes the search input.
    await user.click(screen.getByTestId("outside"));

    expect(
      screen.queryByRole("textbox", {
        name: /providerPreset\.(searchInput|searchPlaceholder)|search presets|search/i,
      }),
    ).not.toBeInTheDocument();
    // Closing clears the query and restores all presets.
    expect(
      screen.getByRole("button", { name: "preset.gamma" }),
    ).toBeInTheDocument();
  });
});
