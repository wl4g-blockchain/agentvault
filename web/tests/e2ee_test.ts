import { beforeAll, describe, expect, it } from 'vitest'
import { encryptWithPublicKey } from '../src/security/e2ee'

function base64Url(value: ArrayBuffer): string {
  return btoa(String.fromCharCode(...new Uint8Array(value)))
    .replaceAll('+', '-').replaceAll('/', '_').replace(/=+$/, '')
}

function bytes(value: string): Uint8Array<ArrayBuffer> {
  return Uint8Array.from(atob(value.replaceAll('-', '+').replaceAll('_', '/')), char => char.charCodeAt(0))
}

describe('E2EE public-key encryption', () => {
  let serverKeys: CryptoKeyPair
  let publicKey: string
  const label = 'purpose-bound-test-payload'

  beforeAll(async () => {
    serverKeys = await crypto.subtle.generateKey(
      { name: 'RSA-OAEP', modulusLength: 3072, publicExponent: new Uint8Array([1, 0, 1]), hash: 'SHA-256' },
      false, ['encrypt', 'decrypt'],
    ) as CryptoKeyPair
    publicKey = base64Url(await crypto.subtle.exportKey('spki', serverKeys.publicKey))
  })

  it('encrypts with the public key and clears the plaintext bytes', async () => {
    const plaintext = new TextEncoder().encode('an arbitrary sensitive payload')
    const expected = plaintext.slice()
    const ciphertext = await encryptWithPublicKey(publicKey, plaintext, label)
    expect([...plaintext]).toEqual(new Array(expected.length).fill(0))
    expect(bytes(ciphertext)).toHaveLength(384)
    const decrypted = new Uint8Array(await crypto.subtle.decrypt(
      { name: 'RSA-OAEP', label: new TextEncoder().encode(label) },
      serverKeys.privateKey, bytes(ciphertext),
    ))
    expect([...decrypted]).toEqual([...expected])
    decrypted.fill(0)
    const second = await encryptWithPublicKey(publicKey, expected, label)
    expect(second).not.toBe(ciphertext)
  })

  it('requires the same purpose label for decryption', async () => {
    const ciphertext = await encryptWithPublicKey(publicKey, new Uint8Array(32).fill(0x22), label)
    await expect(crypto.subtle.decrypt(
      { name: 'RSA-OAEP', label: new TextEncoder().encode('another-purpose') },
      serverKeys.privateKey, bytes(ciphertext),
    )).rejects.toThrow()
  })

  it('clears plaintext when public-key import or encryption fails', async () => {
    for (const [key, length] of [[publicKey, 400], ['invalid', 32]] as const) {
      const plaintext = new Uint8Array(length).fill(0x33)
      await expect(encryptWithPublicKey(key, plaintext, label)).rejects.toThrow()
      expect([...plaintext]).toEqual(new Array(length).fill(0))
    }
  })
})
