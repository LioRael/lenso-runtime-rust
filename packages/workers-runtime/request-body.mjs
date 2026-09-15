export function validateRequestBodyEncoding(encoding) {
  if (encoding !== "numeric-array" && encoding !== "base64-v1")
    throw new TypeError("invalid requestBodyEncoding");
}

export function requestBodyFields(bytes, encoding, limit) {
  if (encoding === "numeric-array") return { body: [...bytes] };

  // readBody enforces the decoded limit before collecting bytes. Bound the
  // encoded allocation too, before converting any byte to a JS string.
  const encodedLength = 4 * Math.ceil(bytes.byteLength / 3);
  if (
    bytes.byteLength > limit ||
    !Number.isSafeInteger(encodedLength) ||
    encodedLength > 4 * Math.ceil(limit / 3)
  ) {
    const error = new Error("payload_too_large");
    error.status = 413;
    throw error;
  }
  const parts = [];
  // A multiple of three keeps padding exclusively in the final chunk. A fixed
  // chunk bounds argument expansion and the transient binary string on Workers.
  for (let offset = 0; offset < bytes.byteLength; offset += 12_288)
    parts.push(
      btoa(String.fromCharCode(...bytes.subarray(offset, offset + 12_288))),
    );
  return { body_encoding: "base64-v1", body_base64: parts.join("") };
}
