#pragma once
// 最小 SHA-256 + HMAC-SHA256 + base64 + hex，header-only（无外部依赖）
#include <cstdint>
#include <cstring>
#include <string>

namespace gopt::sha {

namespace detail {
inline uint32_t rotr(uint32_t x, int n) { return (x >> n) | (x << (32 - n)); }
}  // namespace detail

inline std::string sha256(const std::string& data) {
    static const uint32_t K[64] = {
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1,
        0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
        0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
        0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147,
        0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
        0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
        0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
        0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2};
    uint32_t h[8] = {0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
                     0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19};
    const uint64_t bitLen = static_cast<uint64_t>(data.size()) * 8;

    std::string msg = data;
    msg.push_back(static_cast<char>(0x80));
    while (msg.size() % 64 != 56) msg.push_back('\0');
    for (int i = 7; i >= 0; --i) msg.push_back(static_cast<char>((bitLen >> (i * 8)) & 0xff));

    for (size_t i = 0; i < msg.size(); i += 64) {
        uint32_t w[64];
        for (int t = 0; t < 16; ++t)
            w[t] = (static_cast<uint32_t>(static_cast<uint8_t>(msg[i + 4 * t])) << 24) |
                   (static_cast<uint32_t>(static_cast<uint8_t>(msg[i + 4 * t + 1])) << 16) |
                   (static_cast<uint32_t>(static_cast<uint8_t>(msg[i + 4 * t + 2])) << 8) |
                   static_cast<uint32_t>(static_cast<uint8_t>(msg[i + 4 * t + 3]));
        for (int t = 16; t < 64; ++t) {
            const uint32_t s0 = detail::rotr(w[t - 15], 7) ^ detail::rotr(w[t - 15], 18) ^
                                (w[t - 15] >> 3);
            const uint32_t s1 = detail::rotr(w[t - 2], 17) ^ detail::rotr(w[t - 2], 19) ^
                                (w[t - 2] >> 10);
            w[t] = w[t - 16] + s0 + w[t - 7] + s1;
        }
        uint32_t a = h[0], b = h[1], c = h[2], d = h[3], e = h[4], f = h[5], g = h[6], hh = h[7];
        for (int t = 0; t < 64; ++t) {
            const uint32_t S1 = detail::rotr(e, 6) ^ detail::rotr(e, 11) ^ detail::rotr(e, 25);
            const uint32_t ch = (e & f) ^ (~e & g);
            const uint32_t t1 = hh + S1 + ch + K[t] + w[t];
            const uint32_t S0 = detail::rotr(a, 2) ^ detail::rotr(a, 13) ^ detail::rotr(a, 22);
            const uint32_t maj = (a & b) ^ (a & c) ^ (b & c);
            const uint32_t t2 = S0 + maj;
            hh = g; g = f; f = e; e = d + t1; d = c; c = b; b = a; a = t1 + t2;
        }
        h[0] += a; h[1] += b; h[2] += c; h[3] += d;
        h[4] += e; h[5] += f; h[6] += g; h[7] += hh;
    }
    std::string out;
    out.reserve(32);
    for (int i = 0; i < 8; ++i) {
        out.push_back(static_cast<char>((h[i] >> 24) & 0xff));
        out.push_back(static_cast<char>((h[i] >> 16) & 0xff));
        out.push_back(static_cast<char>((h[i] >> 8) & 0xff));
        out.push_back(static_cast<char>(h[i] & 0xff));
    }
    return out;
}

inline std::string hmac_sha256(const std::string& key, const std::string& msg) {
    const size_t block = 64;
    std::string k = key;
    if (k.size() > block) k = sha256(k);
    k.resize(block, '\0');
    std::string ipad(block, '\x36'), opad(block, '\x5c');
    for (size_t i = 0; i < block; ++i) {
        ipad[i] = static_cast<char>(ipad[i] ^ k[i]);
        opad[i] = static_cast<char>(opad[i] ^ k[i]);
    }
    return sha256(opad + sha256(ipad + msg));
}

inline std::string hex(const std::string& bytes) {
    static const char* d = "0123456789abcdef";
    std::string out;
    for (unsigned char c : bytes) {
        out.push_back(d[c >> 4]);
        out.push_back(d[c & 0xf]);
    }
    return out;
}

inline std::string hex_decode(const std::string& s) {
    auto val = [](char c) -> int {
        if (c >= '0' && c <= '9') return c - '0';
        if (c >= 'a' && c <= 'f') return c - 'a' + 10;
        if (c >= 'A' && c <= 'F') return c - 'A' + 10;
        return -1;
    };
    std::string out;
    for (size_t i = 0; i + 1 < s.size(); i += 2) {
        const int hi = val(s[i]), lo = val(s[i + 1]);
        if (hi < 0 || lo < 0) return {};
        out.push_back(static_cast<char>((hi << 4) | lo));
    }
    return out;
}

inline std::string base64_encode(const std::string& in) {
    static const char tbl[] =
        "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    std::string out;
    int val = 0, bits = -6;
    for (unsigned char c : in) {
        val = (val << 8) + c;
        bits += 8;
        while (bits >= 0) {
            out.push_back(tbl[(val >> bits) & 0x3f]);
            bits -= 6;
        }
    }
    if (bits > -6) out.push_back(tbl[((val << 8) >> (bits + 8)) & 0x3f]);
    while (out.size() % 4) out.push_back('=');
    return out;
}

inline std::string base64_decode(const std::string& in) {
    static const int D[256] = {
        -1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,
        -1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,62,-1,-1,-1,63,52,53,54,55,56,57,58,59,
        60,61,-1,-1,-1,0,-1,-1,-1,0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,
        22,23,24,25,-1,-1,-1,-1,-1,-1,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,
        44,45,46,47,48,49,50,51,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,
        -1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,
        -1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,
        -1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,
        -1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,
        -1,-1,-1,-1};
    std::string out;
    int val = 0, bits = -8;
    for (char c : in) {
        if (c == '=' || c == '\n' || c == '\r' || c == ' ') continue;
        const int d = D[static_cast<unsigned char>(c)];
        if (d < 0) continue;
        val = (val << 6) + d;
        bits += 6;
        if (bits >= 0) {
            out.push_back(static_cast<char>((val >> bits) & 0xff));
            bits -= 8;
        }
    }
    return out;
}

}  // namespace gopt::sha
