import { describe, expect, it, vi } from "vitest";
import { flushPromises, mount } from "@vue/test-utils";
import { createPinia, setActivePinia } from "pinia";

const api = vi.hoisted(() => ({ query: vi.fn() }));
vi.mock("../src/platform/search", async (orig) => ({
  ...(await orig<typeof import("../src/platform/search")>()),
  searchApi: api,
}));

describe("search panel", () => {
  it("stops showing the spinner when the query is cleared while a search is in flight", async () => {
    vi.useFakeTimers();
    setActivePinia(createPinia());
    api.query.mockReturnValue(new Promise(() => {})); // never settles
    const { useSearch } = await import("../src/stores/search");
    const SearchPanel = (await import("../src/components/SearchPanel.vue")).default;
    const search = useSearch();
    search.query = "ink";
    const w = mount(SearchPanel, { attachTo: document.body });
    await vi.advanceTimersByTimeAsync(200);
    expect(api.query).toHaveBeenCalledWith("ink");
    expect(document.body.querySelector(".animate-spin")).not.toBeNull();

    search.query = "";
    await vi.advanceTimersByTimeAsync(200);
    await flushPromises();
    expect(document.body.querySelector(".animate-spin")).toBeNull();
    w.unmount();
    vi.useRealTimers();
  });
});
