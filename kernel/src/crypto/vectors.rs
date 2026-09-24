//! Test vectors from other implementations.
//!
//! Every value here was produced by something that is not this kernel --
//! `cryptography` (OpenSSL) for the signatures, CPython's pow() for the
//! modular exponentiation, the standards documents for the ciphers.  That
//! distinction is the whole point: a primitive checked only against its own
//! output passes with the limbs reversed, the Montgomery constant wrong, or
//! the padding not checked at all.
//!
//! The RSA key is generated once and pinned, so the vector is reproducible
//! without shipping a private key: only the public half is here, and only the
//! signatures need it.

/// The message everything RSA-signed below signs.  ASCII, so it reads in a
/// hex dump.
pub const RSA_MSG: &[u8] = b"barryOS TLS test vector, signed by an independent implementation.";



pub const RSA_N: &[u8] = b"d34e2031166cfa49dfc0312ffd0929903ca102ce1cf9f0fa4d5846c957727ca4
    445d43ec929fc6368e63bb14a1fadb977a62d6ee8e759567d59db6a2843e8e74
    37a2a0ffde2a47606b68352a68f9d8812829b672bc9ef75eccdb21fd45ee8811
    dbd9f774e99c3643f162481784efcdd174024ad467f8b8e895512ba3233157d1
    f6bffc60d071053bd1913fb1a9246c35078bdde2c170c3dd93424acc0cb53378
    569c125d76d5c0d0d793be53c878684074aa735148534fc7e94beba9a19f7672
    1e6b1d567f5bfc0bb658c56c056da726c3a4ca85be62369c371f16676ad45b03
    dc44b3a820974059b7d74bc181d784ce30b0916e38ce59997868213fdbf7dbfd
";

pub const RSA_E: &[u8] = b"010001";

pub const RSA_PKCS1_SHA256_SIG: &[u8] = b"afd1de7b63044c4bfdfd599d75026e4aa38c4d08ec2efa907109235b96d50356
    1ca74d0bc6e6964708d3504e24c526c5df7d60a82a5329e630c7a61c01f956bb
    d3b292fd754aa0c0f0f245e447be20628b9531f32599aff609aa817f08031e47
    e9376ff9a6a4a9f96471a1ef491e11e493b857609ea413b4abf9d247657afae4
    7d631ff16272eb1e383a2af8db779aba0b0f8807842039abb1b0fdf23bc504da
    a36d47a779d277522c1f5f24b663cceabc202d8e94583d13f70ae2f847383fe5
    b97b9ec8e1809be690cd30fa9486e3b27c2433e1c3c04958af209a0903809ffc
    239679945b638972ce0f0cee55fa9ffb4f4e0e5ca3c3a9bce3f4af7f42d62189
";

pub const RSA_PSS_SHA256_SIG: &[u8] = b"17c89aade0b54e38eb888b4e0f300dc723388d10fe6ad8d2cd2424dbf8ba1c60
    b0293a63749be54fd2273448baeb3e0381ead3ef8a0e33b1262f6da7dec3cbb3
    26916dce2b32d41b2458391327768f815a01d41b3ca87aeb51b9963c6af93509
    db357549ef59bc2ebd4a824c2b1281c8ab71d3a3c6209202e0dcaadba4919850
    5636363bf39897e2764d7163f73a1b3714d288fc1ba2ab557e48cdcc9b510375
    14dbe86126d4bb03672b8e43fa7c29cc6276c2f5a163aaafb5c894c326f5e1f8
    dd7f383aad6f2cb053e18bd461b6660e31d3de7b3c8bb40beed904f5b44b0f06
    d620ed4e9c88d422d664ec7001b47897730c0996aead287002e14327ee805d9a
";

pub const RSA_PKCS1_SHA384_SIG: &[u8] = b"44285fdd0e8abeba8cb40bcb794d70a65f1dfa999e24bc6ddccb2f4ccea99507
    d6f75bc5352813904952891f93f0ae432b1ec641be7561ac1b3f69856d4a195f
    ae256e143905d3f8d19b674bc74f967a1883aa41897e0616e23d5c0829b2d7c2
    292a3df58b2b5cc64ed624f91e8dc5755720c01935a86faa8792678c98badd79
    144ba53aadd27dc9dbaba06cfa3ee666ee5d5c987b29afda9f44329a3aca6749
    f0ac0e1b15663c88b23f379cb192501fce6ecf6f244dcdae5d628ea688586fe4
    a8387c28b446161f6d7b8ecd882ce1e5883c89c1ae9030e1583cd63e87797c4b
    442e15280c68f501c14ab53537a3397286c7ac31d7661a538444c6622d900589
";

pub const RSA_PSS_SHA384_SIG: &[u8] = b"0590d0c5479d2be07703407a6be25bf6805004c2414a9e8e37720a2debba8c10
    82fbbf34dedd93fb388aaa6043546f4d16c30eacbafec67d5cc1a3368ea0ac29
    95f90fdd3d0af5bc6016624aeee6575b296a30527520c75da909540119b07243
    bd55e2e2aaffceca14a0fccf199f15a7a44c63b430b19c9008385eb77131a88e
    250fe435eb908c11aa014f7ece2d1a725ae3d702b58616e42875e174fc18539d
    96826eb94e219aa86b452aa4d377c91fe54510acfbe85c7e9914a00b262f3cf6
    0f9f0bfbbb7ead09ddd19ebd0108e13348671765f6f0f377b9e0edef125f73da
    364e55ba10cd5870b68fbe0d1103ccd7b81825b494e0782d8ce6e322f5e12145
";

pub const RSA_PKCS1_SHA512_SIG: &[u8] = b"8dba0c1a26ac01e888dedf389732034cf4e545a6939bbd57d52a4b6523bebb87
    09006557a1fc844541767447d541149c254ffb1722bedef3fe317d4ebdb7cb76
    b46234a39150e77dc6ddcf03c9862e167ce64eb05c74fadfaa94852192d9c0d8
    1d8ac1f8e0434ce1ebf5076aa54bc069df18d2dcf75c6dd3fb236fb362eba597
    2077b6246bbcf125fbabc87cc39f63f4336cbead636ebe8fe135fbb5ddd84536
    4d013e4319a56b6943526234b63a00f68babd42172a41660272730b07dc2bfda
    1896306b75a3af062f853cad6f994d3ebb597f9f6c66e40bc434688cc06a91f2
    25225f44a82a3afd4594c881bf4bba6dad5e322891697b1465fce9e44c16701b
";

pub const RSA_PSS_SHA512_SIG: &[u8] = b"7a22cae53806f319e418604b5f1896275ed3e37c932f0c651553b267eee123f1
    66596413255ce3bb845f5e465f7600636d67459d64aa4d4cdcebd767faa39395
    843fb57812c939a93015d79d76957b0a42fc339c2fe1e8773488e79bed5d4e0f
    79a6e2e985ff80adc72f22455ad60d9acec48173176113816a1e187a006b4bc8
    60cdfb74938a2069d04714bdc3922c52d11bb9dcf5c800fabd5d4dcc30ea6818
    c5cede33b9c9215d53d0f64ce0ec9a22f6b2f8d57f73834aceb951986f200180
    f26fa2cb60de3160fade8ea10963befc3908a255f093120b614ef73ceee96548
    0c64f3db4fcff8e0357af2bf8dadc8deae0d97aff13f227c7e58c1f10555e3db
";

pub const RSA_PKCS1_SHA256_SIG_TAMPERED: &[u8] = b"afd1de7b63044c4bfdfd599d75026e4aa38c4d08ec2efa907109235b96d50356
    1ca74d0bc6e6964708d3504e24c526c5df7d60a82a5329e630c7a61c01f956bb
    d3b292fd754aa0c0f0f245e447be20628b9531f32599aff609aa817f08031e47
    e9376ff9a6a4a9f96471a1ef491e11e493b857609ea413b4abf9d247657afae4
    7c631ff16272eb1e383a2af8db779aba0b0f8807842039abb1b0fdf23bc504da
    a36d47a779d277522c1f5f24b663cceabc202d8e94583d13f70ae2f847383fe5
    b97b9ec8e1809be690cd30fa9486e3b27c2433e1c3c04958af209a0903809ffc
    239679945b638972ce0f0cee55fa9ffb4f4e0e5ca3c3a9bce3f4af7f42d62189
";

pub const RSA_PSS_SHA256_SIG_TAMPERED: &[u8] = b"917511e7801830533c1941bff68bc8e72fba1db22d190ba74cd3d1c2e160cdb7
    2dbaf914434b1cf4933d9d8ca10ef2ef83af4565d21de33b971fecdfbca86f6e
    5dc65c0826012098fb4181861cf4b917e7c96d74b6eb6cfb5a5aaf32b263936b
    dbc6b75b77ce2137a1291992055f70ff2e4758db519eab5ba0b74d2938ff61b2
    2735fbd15f8b48a167ec2cc5792f414abf9e8aefed216c23027fe562af50a9ae
    36ecfe67383930b16cd7ac89d3d6c1a14199b558fa75757438152b209d63207b
    7de0465f313202540751c1a6ceddbd8e282512d61271bddb62a3e7de39040ee4
    95065fb316764701243e06f0f3b33db6a6c08cb6e860b3d0930764c3e2c1a315
";

/// TLS 1.2's PRF (RFC 5246 §5), checked against the same construction written
/// out in Python.
///
/// The second case expands 32 bytes into 96, so the A(i) chain has to run past
/// one block — a PRF that stops after the first HMAC, or that forgets to feed
/// A(i-1) forward, matches the first case and not the second.
pub struct Tls12PrfVector {
    pub name: &'static str,
    /// The secret, as HMAC key.
    pub secret: &'static [u8],
    /// The label, hashed into the seed as an ASCII string.
    pub label: &'static str,
    /// The seed that follows the label.
    pub seed: &'static [u8],
    /// The expected output.
    pub want: &'static [u8],
}

pub const TLS12_PRF: &[Tls12PrfVector] = &[
    Tls12PrfVector {
        name: "tls 1.2 prf, one block",
        secret: b"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f202122232425262728292a2b2c2d2e2f",
        label: "master secret",
        seed: b"01080f161d242b323940474e555c636a71787f868d949ba2a9b0b7bec5ccd3dae1e8eff6fd040b121920272e353c434a51585f666d747b828990979ea5acb3ba",
        want: b"1e193a96f6e9cd8dc4cf7903f019aac556dbc81710d5258dd2bccd0a2eb1cb01361abc68e28fbd1c6e3cc4abcb7c9341",
    },
    Tls12PrfVector {
        name: "tls 1.2 prf, across blocks",
        secret: b"030e19242f3a45505b66717c87929da8b3bec9d4dfeaf5000b16212c37424d58",
        label: "key expansion",
        seed: b"05121f2c394653606d7a8794a1aebbc8d5e2effc091623303d4a5764717e8b98a5b2bfccd9e6f3000d1a2734414e5b6875828f9ca9b6c3d0ddeaf704111e2b38",
        want: b"489ced884d8b3df9c3a6920e0a849b21f37c37147b9998647d683c205392165a44f50711a4fe81d0c02c58bd4f757583acd200255a1e9f8b625699028dfd11ad3f7dbb46ff02f0d5b7f2ad98e65038373fd314dac956d88c98bb303b43809b5c",
    },
];
