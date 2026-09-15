/**
 * Tests for the BroadcastBanner component.
 *
 * Verifies rendering, watch button interaction, and dismiss behaviour.
 */
import { beforeEach, describe, it, expect, vi } from "vitest";
import { render as renderComponent, screen, fireEvent } from "@testing-library/react";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import type { ReactElement } from "react";
import enChat from "../../locales/en/chat.json";
import deChat from "../../locales/de/chat.json";
import frChat from "../../locales/fr/chat.json";
import zhChat from "../../locales/zh/chat.json";
import enCommon from "../../locales/en/common.json";
import { BroadcastBanner } from "../chat/stream/ScreenShareViewer";

vi.unmock("react-i18next");

const i18n = createInstance();

function render(ui: ReactElement) {
  return renderComponent(<I18nextProvider i18n={i18n}>{ui}</I18nextProvider>);
}

beforeEach(async () => {
  await i18n.init({
    lng: "en",
    fallbackLng: "en",
    interpolation: { escapeValue: false },
    resources: {
      en: { chat: enChat, common: enCommon },
      de: { chat: deChat },
      fr: { chat: frChat },
      zh: { chat: zhChat },
    },
  });
});

describe("BroadcastBanner", () => {
  it.each([
    ["en", "Alice is sharing their screen"],
    ["de", "Alice teilt den Bildschirm"],
    ["fr", "Alice partage son écran"],
    ["zh", "Alice 正在共享屏幕"],
  ])("renders the translated message once in %s", async (language, expected) => {
    await i18n.changeLanguage(language);
    render(
      <BroadcastBanner
        broadcasters={[{ session: 42, name: "Alice" }]}
        onWatch={vi.fn()}
      />,
    );
    const name = screen.getByText("Alice");
    expect(name.className).toContain("broadcastBannerName");
    expect(name.parentElement?.textContent).toBe(expected);
    expect(screen.getByRole("status").textContent).not.toContain("{{name}}");
  });

  it("renders markup-like broadcaster names as literal text", () => {
    const name = "<b>Alice</b> & {{name}}";
    render(
      <BroadcastBanner
        broadcasters={[{ session: 42, name }]}
        onWatch={vi.fn()}
      />,
    );
    const label = screen.getByText(name);
    expect(label.parentElement?.textContent).toBe(`${name} is sharing their screen`);
    expect(label.querySelector("b")).toBeNull();
  });

  it("renders nothing when no broadcasters", () => {
    const { container } = render(
      <BroadcastBanner broadcasters={[]} onWatch={vi.fn()} />,
    );
    expect(container.innerHTML).toBe("");
  });

  it("shows broadcaster name", () => {
    render(
      <BroadcastBanner
        broadcasters={[{ session: 42, name: "Alice" }]}
        onWatch={vi.fn()}
      />,
    );
    expect(screen.getByText("Alice")).toBeTruthy();
    expect(screen.getByText(/sharing their screen/)).toBeTruthy();
  });

  it("shows multiple broadcaster banners", () => {
    render(
      <BroadcastBanner
        broadcasters={[
          { session: 1, name: "Alice" },
          { session: 2, name: "Bob" },
        ]}
        onWatch={vi.fn()}
      />,
    );
    expect(screen.getByText("Alice")).toBeTruthy();
    expect(screen.getByText("Bob")).toBeTruthy();
  });

  it("calls onWatch with the correct session when Watch is clicked", () => {
    const onWatch = vi.fn();
    render(
      <BroadcastBanner
        broadcasters={[{ session: 42, name: "Alice" }]}
        onWatch={onWatch}
      />,
    );
    fireEvent.click(screen.getByText("Watch"));
    expect(onWatch).toHaveBeenCalledWith(42);
  });

  it("dismisses a banner when dismiss button is clicked", () => {
    render(
      <BroadcastBanner
        broadcasters={[
          { session: 1, name: "Alice" },
          { session: 2, name: "Bob" },
        ]}
        onWatch={vi.fn()}
      />,
    );

    // Dismiss Alice's banner.
    const dismissButtons = screen.getAllByTitle("Dismiss");
    fireEvent.click(dismissButtons[0]);

    // Alice's banner should be gone, Bob's should remain.
    expect(screen.queryByText("Alice")).toBeNull();
    expect(screen.getByText("Bob")).toBeTruthy();
  });

  it("shows P2P label when sfuAvailable is false", () => {
    render(
      <BroadcastBanner
        broadcasters={[{ session: 1, name: "Alice" }]}
        onWatch={vi.fn()}
        sfuAvailable={false}
      />,
    );
    expect(screen.getByText("P2P")).toBeTruthy();
  });

  it("does not show P2P label when sfuAvailable is true", () => {
    render(
      <BroadcastBanner
        broadcasters={[{ session: 1, name: "Alice" }]}
        onWatch={vi.fn()}
        sfuAvailable={true}
      />,
    );
    expect(screen.queryByText("P2P")).toBeNull();
  });

  it("does not show P2P label when sfuAvailable is omitted (default)", () => {
    render(
      <BroadcastBanner
        broadcasters={[{ session: 1, name: "Alice" }]}
        onWatch={vi.fn()}
      />,
    );
    expect(screen.queryByText("P2P")).toBeNull();
  });
});
