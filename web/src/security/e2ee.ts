// Fixed wire contract: RSA-3072 OAEP, SHA-256 for both OAEP and MGF1.
// Consumes plaintext bytes and clears them on both success and failure.
export async function encryptWithPublicKey(
  serverPublicKey: string,
  plaintext: Uint8Array<ArrayBuffer>,
  label: string,
): Promise<string> {
  try {
    const publicKey = await crypto.subtle.importKey(
      'spki', decodeBase64Url(serverPublicKey),
      { name: 'RSA-OAEP', hash: 'SHA-256' }, false, ['encrypt'],
    )
    const ciphertext = await crypto.subtle.encrypt(
      {
        name: 'RSA-OAEP',
        label: new TextEncoder().encode(label),
      },
      publicKey,
      plaintext,
    )
    return encodeBase64Url(new Uint8Array(ciphertext))
  } finally {
    plaintext.fill(0)
  }
}

function decodeBase64Url(value: string): Uint8Array<ArrayBuffer> {
  if (!/^[A-Za-z0-9_-]+$/.test(value)) throw new Error('Public key encoding is invalid.')
  const padded = value.replaceAll('-', '+').replaceAll('_', '/') + '='.repeat((4 - value.length % 4) % 4)
  const bytes = Uint8Array.from(atob(padded), char => char.charCodeAt(0))
  if (encodeBase64Url(bytes) !== value) throw new Error('Public key encoding is not canonical.')
  return bytes
}

function encodeBase64Url(value: Uint8Array): string {
  return btoa(String.fromCharCode(...value)).replaceAll('+', '-').replaceAll('/', '_').replace(/=+$/, '')
}
