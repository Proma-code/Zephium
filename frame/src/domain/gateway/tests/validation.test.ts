import { expect, test } from "vitest";
import { gatewayBaseValid, gatewayKeyValid, gatewayLabelValid } from "../validation";
import { gatewayCheckedAt, gatewayFault, gatewayStatus } from "../presentation";
import { gatewayView } from "$shared/testing/gateway";

test.each(["http://127.0.0.1:1/v1", "http://127.0.0.1:65535/v1/", "http://[::1]:8080/v1"])(
  "accepts the exact literal loopback form: %s",
  (base) => expect(gatewayBaseValid(base)).toBe(true),
);

test.each([
  "http://localhost:8080/v1",
  "http://127.1:8080/v1",
  "http://2130706433:8080/v1",
  "http://127.0.0.2:8080/v1",
  "https://127.0.0.1:8080/v1",
  "http://192.168.1.1:8080/v1",
  "http://[::ffff:127.0.0.1]:8080/v1",
  "http://[0:0:0:0:0:0:0:1]:8080/v1",
  "http://127.0.0.1:0/v1",
  "http://127.0.0.1:65536/v1",
  "http://127.0.0.1:080/v1",
  "http://127.0.0.1/v1",
  "http://user:secret@127.0.0.1:8080/v1",
  "http://127.0.0.1:8080/v1?key=x",
  "http://127.0.0.1:8080/v1#x",
  "http://127.0.0.1:8080/v1//",
  "http://127.0.0.1:8080/v1/../",
  " http://127.0.0.1:8080/v1",
  "http://127.0.0.1:8080/v1\n",
  "HTTP://127.0.0.1:8080/v1",
])("rejects noncanonical or broader network destinations: %s", (base) =>
  expect(gatewayBaseValid(base)).toBe(false),
);

test("labels are bounded in UTF-8 bytes and keys have the native ASCII-only bounds", () => {
  expect(gatewayLabelValid("Local gateway")).toBe(true);
  expect(gatewayLabelValid("é".repeat(32))).toBe(true);
  for (const label of ["", "é".repeat(33), "x".repeat(65), " local", "local ", "a\nb", "a\u007fb"])
    expect(gatewayLabelValid(label)).toBe(false);
  expect(gatewayKeyValid("a".repeat(1024))).toBe(true);
  expect(gatewayKeyValid("!~")).toBe(true);
  for (const key of ["", "a".repeat(1025), "key key", " key", "key\n", "é", "\u007f"])
    expect(gatewayKeyValid(key)).toBe(false);
});

test("presentation never equates saved settings, a catalog check, or unknown quota with inference", () => {
  const view = gatewayView("profile");
  expect(gatewayStatus(view)).toContain("No local gateway");
  expect(gatewayStatus({ ...view, state: "configured" })).toBe("Configured — catalog not checked");
  expect(gatewayStatus({ ...view, state: "checked", models: 2 })).toBe(
    "Catalog checked · 2 models",
  );
  expect(gatewayFault("usage_unavailable")).toBe("Gateway quota and pricing are not reported.");
  expect(gatewayCheckedAt({ ...view, checked_at_ms: "1700000000000" })).not.toBeNull();
  for (const checked_at_ms of [null, "nan", "-1", "9999999999999999"])
    expect(gatewayCheckedAt({ ...view, checked_at_ms })).toBeNull();
});
