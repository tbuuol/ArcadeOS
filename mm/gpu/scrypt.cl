// scrypt(N=1024, r=1, p=1) ueber einen 80-Byte-Blockkopf (Litecoin/Dogecoin/Pepecoin-PoW), ein Hash pro Work-Item
__constant uint K[64] = {
0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,
0xe49b69c1,0xefbe4786,0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,
0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,0x81c2c92e,0x92722c85,0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,
0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,0x748f82ee,0x78a5636f,0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2};
#define ROTR(x,n) rotate((uint)(x),(uint)(32-(n)))
typedef struct { uint s[8]; uchar buf[64]; uint len; uint total; } sha_t;
void sha_block(uint *s, const uchar *b) {
  uint w[64];
  for (int i = 0; i < 16; i++) w[i] = (uint)b[4*i]<<24 | (uint)b[4*i+1]<<16 | (uint)b[4*i+2]<<8 | b[4*i+3];
  for (int i = 16; i < 64; i++) { uint s0 = ROTR(w[i-15],7)^ROTR(w[i-15],18)^(w[i-15]>>3), s1 = ROTR(w[i-2],17)^ROTR(w[i-2],19)^(w[i-2]>>10); w[i] = w[i-16]+s0+w[i-7]+s1; }
  uint a=s[0],b2=s[1],c=s[2],d=s[3],e=s[4],f=s[5],g=s[6],h=s[7];
  for (int i = 0; i < 64; i++) {
    uint t1 = h + (ROTR(e,6)^ROTR(e,11)^ROTR(e,25)) + ((e&f)^(~e&g)) + K[i] + w[i];
    uint t2 = (ROTR(a,2)^ROTR(a,13)^ROTR(a,22)) + ((a&b2)^(a&c)^(b2&c));
    h=g; g=f; f=e; e=d+t1; d=c; c=b2; b2=a; a=t1+t2;
  }
  s[0]+=a; s[1]+=b2; s[2]+=c; s[3]+=d; s[4]+=e; s[5]+=f; s[6]+=g; s[7]+=h;
}
void sha_init(sha_t *c) { c->s[0]=0x6a09e667; c->s[1]=0xbb67ae85; c->s[2]=0x3c6ef372; c->s[3]=0xa54ff53a; c->s[4]=0x510e527f; c->s[5]=0x9b05688c; c->s[6]=0x1f83d9ab; c->s[7]=0x5be0cd19; c->len=0; c->total=0; }
void sha_upd(sha_t *c, const uchar *d, uint n) { for (uint i = 0; i < n; i++) { c->buf[c->len++] = d[i]; if (c->len == 64) { sha_block(c->s, c->buf); c->len = 0; } } c->total += n; }
void sha_fin(sha_t *c, uchar *out) {
  ulong bits = (ulong)c->total * 8; uchar p = 0x80; sha_upd(c, &p, 1); p = 0;
  while (c->len != 56) sha_upd(c, &p, 1);
  for (int i = 7; i >= 0; i--) { uchar x = (uchar)(bits >> (8*i)); c->buf[c->len++] = x; }
  sha_block(c->s, c->buf);
  for (int i = 0; i < 8; i++) { out[4*i]=c->s[i]>>24; out[4*i+1]=c->s[i]>>16; out[4*i+2]=c->s[i]>>8; out[4*i+3]=c->s[i]; }
}
// PBKDF2-HMAC-SHA256, 1 Runde; Schluessel = 80-Byte-Kopf
void pbkdf2(const uchar *pw, const uchar *salt, uint slen, uchar *out, uint olen) {
  uchar kh[32], ip[64], op[64], u[32]; sha_t c;
  sha_init(&c); sha_upd(&c, pw, 80); sha_fin(&c, kh);
  for (int i = 0; i < 64; i++) { uchar k = i < 32 ? kh[i] : 0; ip[i] = k ^ 0x36; op[i] = k ^ 0x5c; }
  for (uint blk = 1; blk * 32 <= olen; blk++) {
    uchar be[4] = {0, 0, 0, (uchar)blk};
    sha_init(&c); sha_upd(&c, ip, 64); sha_upd(&c, salt, slen); sha_upd(&c, be, 4); sha_fin(&c, u);
    sha_init(&c); sha_upd(&c, op, 64); sha_upd(&c, u, 32); sha_fin(&c, out + (blk-1)*32);
  }
}
#define R(a,b) rotate((uint)(a),(uint)(b))
void salsa8(uint *B, const uint *Bx) {
  uint x[16];
  for (int i = 0; i < 16; i++) x[i] = (B[i] ^= Bx[i]);
  for (int i = 0; i < 8; i += 2) {
    x[4]^=R(x[0]+x[12],7);  x[8]^=R(x[4]+x[0],9);   x[12]^=R(x[8]+x[4],13);  x[0]^=R(x[12]+x[8],18);
    x[9]^=R(x[5]+x[1],7);   x[13]^=R(x[9]+x[5],9);  x[1]^=R(x[13]+x[9],13);  x[5]^=R(x[1]+x[13],18);
    x[14]^=R(x[10]+x[6],7); x[2]^=R(x[14]+x[10],9); x[6]^=R(x[2]+x[14],13);  x[10]^=R(x[6]+x[2],18);
    x[3]^=R(x[15]+x[11],7); x[7]^=R(x[3]+x[15],9);  x[11]^=R(x[7]+x[3],13);  x[15]^=R(x[11]+x[7],18);
    x[1]^=R(x[0]+x[3],7);   x[2]^=R(x[1]+x[0],9);   x[3]^=R(x[2]+x[1],13);   x[0]^=R(x[3]+x[2],18);
    x[6]^=R(x[5]+x[4],7);   x[7]^=R(x[6]+x[5],9);   x[4]^=R(x[7]+x[6],13);   x[5]^=R(x[4]+x[7],18);
    x[11]^=R(x[10]+x[9],7); x[8]^=R(x[11]+x[10],9); x[9]^=R(x[8]+x[11],13);  x[10]^=R(x[9]+x[8],18);
    x[12]^=R(x[15]+x[14],7);x[13]^=R(x[12]+x[15],9);x[14]^=R(x[13]+x[12],13);x[15]^=R(x[14]+x[13],18);
  }
  for (int i = 0; i < 16; i++) B[i] += x[i];
}
__kernel void scrypt(__global const uint *hdr, uint nonce0, __global uint *V, __global uint *out) {
  uint gid = get_global_id(0);
  uchar h[80], xb[128], hash[32];
  for (int i = 0; i < 19; i++) { uint w = hdr[i]; h[4*i]=w; h[4*i+1]=w>>8; h[4*i+2]=w>>16; h[4*i+3]=w>>24; }
  uint n = nonce0 + gid; h[76]=n; h[77]=n>>8; h[78]=n>>16; h[79]=n>>24;
  pbkdf2(h, h, 80, xb, 128);
  uint X[32];
  for (int i = 0; i < 32; i++) X[i] = (uint)xb[4*i] | (uint)xb[4*i+1]<<8 | (uint)xb[4*i+2]<<16 | (uint)xb[4*i+3]<<24;
#ifndef LG
#define LG 1
#endif
  // Lookup-Gap: nur jeder LG-te Zustand liegt im Speicher, die Zwischenschritte werden neu gerechnet (weniger Speicherverkehr)
  __global uint4 *v = (__global uint4 *)(V + (size_t)gid * (32768 / LG));
  for (int i = 0; i < 1024; i++) {
    if (i % LG == 0) for (int k = 0; k < 8; k++) v[(i/LG)*8+k] = (uint4)(X[4*k], X[4*k+1], X[4*k+2], X[4*k+3]);
    salsa8(X, X + 16); salsa8(X + 16, X);
  }
  for (int i = 0; i < 1024; i++) {
    uint j = X[16] & 1023;
    uint Y[32];
    for (int k = 0; k < 8; k++) { uint4 t = v[(j/LG)*8+k]; Y[4*k]=t.x; Y[4*k+1]=t.y; Y[4*k+2]=t.z; Y[4*k+3]=t.w; }
    for (uint r = 0; r < j % LG; r++) { salsa8(Y, Y + 16); salsa8(Y + 16, Y); }
    for (int k = 0; k < 32; k++) X[k] ^= Y[k];
    salsa8(X, X + 16); salsa8(X + 16, X);
  }
  for (int i = 0; i < 32; i++) { xb[4*i]=X[i]; xb[4*i+1]=X[i]>>8; xb[4*i+2]=X[i]>>16; xb[4*i+3]=X[i]>>24; }
  pbkdf2(h, xb, 128, hash, 32);
  for (int i = 0; i < 8; i++) out[gid*8+i] = (uint)hash[4*i] | (uint)hash[4*i+1]<<8 | (uint)hash[4*i+2]<<16 | (uint)hash[4*i+3]<<24;
}
