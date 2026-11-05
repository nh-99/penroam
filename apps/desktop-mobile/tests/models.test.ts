import { describe, expect, it } from "vitest";
import { modelNotice, modelToOffer, type ModelsStatus, type OfferedModel } from "../src/platform/models";
import type { ModelSummary } from "../src/platform/search";

const offered: OfferedModel = { id: "hw-en", version: "2", name: "M", license: "MIT", languages: ["en"], bytes: 10 };
const installed = (id: string, version: string): ModelSummary => ({ id, version, name: "M", license: "", languages: [], bytes: 1, provenance: null });
const status = (o: Partial<ModelsStatus>): ModelsStatus => ({
  enabled: true,
  phase: "idle",
  automatic: false,
  received: 0,
  total: 0,
  checked: false,
  available: null,
  error: null,
  ...o,
});

describe("modelToOffer", () => {
  it("offers a model when none is installed", () => {
    expect(modelToOffer(offered, null)).toBe(offered);
  });
  it("offers nothing when the installed model is the recommended one", () => {
    expect(modelToOffer(offered, installed("hw-en", "2"))).toBeNull();
  });
  it("offers a different version, older ones included: the catalog can roll a model back", () => {
    expect(modelToOffer(offered, installed("hw-en", "3"))).toBe(offered);
  });
  it("offers a replacement model with another id", () => {
    expect(modelToOffer(offered, installed("hw-old", "2"))).toBe(offered);
  });
  it("offers nothing without a catalog", () => {
    expect(modelToOffer(null, installed("hw-en", "2"))).toBeNull();
  });
});

describe("modelNotice", () => {
  it("announces the first-run download once, when it starts", () => {
    const checking = status({ phase: "checking", automatic: true });
    const downloading = status({ phase: "downloading", automatic: true });
    expect(modelNotice(checking, downloading)).toContain("Downloading");
    expect(modelNotice(downloading, downloading)).toBeNull();
  });
  it("stays quiet when the user started the download", () => {
    expect(modelNotice(status({ phase: "checking" }), status({ phase: "downloading" }))).toBeNull();
  });
  it("reports a finished install, and not a failed one", () => {
    const installing = status({ phase: "installing" });
    expect(modelNotice(installing, status({}))).toContain("installed");
    expect(modelNotice(installing, status({ error: "no" }))).toBeNull();
  });
  it("stays quiet about a first-run attempt that failed", () => {
    expect(modelNotice(status({ phase: "downloading", automatic: true }), status({ automatic: true }))).toBeNull();
  });
});
