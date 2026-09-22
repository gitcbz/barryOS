// Host-side check for the kernel's SHA-256.
//
// The kernel build cannot run the code, so the implementation is compiled here
// with the ordinary host toolchain and checked against the published FIPS
// 180-4 vectors.  Run it after touching sha256.rs:
//
//     rustc -O --test tests/sha256-host.rs -o /tmp/sha256-test && /tmp/sha256-test
// or simply:  rustc -O tests/sha256-host.rs -o t && ./t

// Loaded with #[path] rather than include! so the module's own `//!` docs are
// allowed where they are.
#[path = "../kernel/src/sha256.rs"]
mod sha256;

use sha256::{constant_time_eq, hash, hash_hex, Sha256};

const CASES: [(&[u8], &str); 6] = [
    (b"", "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"),
    (b"abc", "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
    (
        b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
    ),
    (
        b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu",
        "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1",
    ),
    // 55 and 56 bytes straddle the point where the length field no longer
    // fits in the first padding block — the classic off-by-one.
    (
        b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318",
    ),
    (
        b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a",
    ),
];

fn main() {
    let mut failures = 0;

    for (input, want) in CASES.iter() {
        let mut got = [0u8; 64];
        hash_hex(input, &mut got);
        let got = core::str::from_utf8(&got).unwrap();
        if got == *want {
            println!("  ok   len={:<4} {}", input.len(), &got[..16]);
        } else {
            println!("  FAIL len={:<4}\n       want {}\n       got  {}", input.len(), want, got);
            failures += 1;
        }
    }

    // One million 'a's, fed in small pieces: checks the streaming path and the
    // 64-bit length field.
    let mut s = Sha256::new();
    let chunk = [b'a'; 1000];
    for _ in 0..1000 {
        s.update(&chunk);
    }
    let mut got = [0u8; 64];
    {
        let digest = s.finish();
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for (i, b) in digest.iter().enumerate() {
            got[i * 2] = HEX[(b >> 4) as usize];
            got[i * 2 + 1] = HEX[(b & 0xF) as usize];
        }
    }
    let got = core::str::from_utf8(&got).unwrap();
    let want = "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0";
    if got == want {
        println!("  ok   1e6 x 'a' (streamed in 1000-byte pieces)");
    } else {
        println!("  FAIL 1e6 x 'a'\n       want {}\n       got  {}", want, got);
        failures += 1;
    }

    // Constant-time compare must not be fooled by a prefix match.
    let a = hash(b"one");
    let b = hash(b"two");
    assert!(constant_time_eq(&a, &a));
    assert!(!constant_time_eq(&a, &b));
    assert!(!constant_time_eq(&a, &a[..31]));
    println!("  ok   constant_time_eq");

    if failures == 0 {
        println!("\nall SHA-256 checks passed");
    } else {
        println!("\n{} FAILED", failures);
        std::process::exit(1);
    }
}
