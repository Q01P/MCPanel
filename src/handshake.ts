import type { ServerHandshake } from "./types";

/** Capability keys in the order the spec lists them; anything else a
 * server advertises is appended as-is so nothing is hidden. */
const KNOWN_CAPABILITIES = ["tools", "resources", "prompts", "logging", "completions"];

/** The capability names a server advertised, spec order first. */
export function capabilityNames(handshake: ServerHandshake): string[] {
  const advertised = Object.keys(handshake.capabilities ?? {});
  const known = KNOWN_CAPABILITIES.filter((name) => advertised.includes(name));
  const rest = advertised.filter((name) => !KNOWN_CAPABILITIES.includes(name)).sort();
  return [...known, ...rest];
}

export function hasCapability(handshake: ServerHandshake | null, name: string): boolean {
  return handshake != null && Object.hasOwn(handshake.capabilities ?? {}, name);
}

/** A server's sub-capability flag, e.g. `tools.listChanged`. */
export function capabilityFlag(
  handshake: ServerHandshake | null,
  capability: string,
  flag: string,
): boolean {
  const value = handshake?.capabilities?.[capability];
  return (
    value != null &&
    typeof value === "object" &&
    (value as Record<string, unknown>)[flag] === true
  );
}

/** "name version" as the server identifies itself, or null when it sent
 * no usable serverInfo. */
export function serverIdentity(handshake: ServerHandshake): string | null {
  const info = handshake.server_info ?? {};
  const name = typeof info.name === "string" ? info.name.trim() : "";
  const version = typeof info.version === "string" ? info.version.trim() : "";
  if (name === "" && version === "") return null;
  return name === "" ? version : version === "" ? name : `${name} ${version}`;
}

/** One line for the server row: identity, protocol revision, capabilities. */
export function describeHandshake(handshake: ServerHandshake): string {
  const parts: string[] = [];
  const identity = serverIdentity(handshake);
  if (identity) parts.push(identity);
  if (handshake.protocol_version) parts.push(`MCP ${handshake.protocol_version}`);
  const capabilities = capabilityNames(handshake);
  parts.push(capabilities.length > 0 ? capabilities.join(", ") : "no capabilities advertised");
  return parts.join(" · ");
}
