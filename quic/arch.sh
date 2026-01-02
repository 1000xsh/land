# solana will not compile without "enable-ssl3-method"
# sudo git clone https://github.com/quictls/openssl.git quictls-openssl
# ./Configure linux-x86_64 --prefix=/opt/quictls --libdir=lib enable-tls1_3 enable-ktls enable-ssl3-method no-deprecated
# make -j"$(nproc)"
# sudo make install
# export LD_LIBRARY_PATH=/opt/quictls/lib:$LD_LIBRARY_PATH
# export PATH=/opt/quictls/bin:$PATH
# openssl version
# export PKG_CONFIG_PATH=/opt/quictls/lib/pkgconfig:$PKG_CONFIG_PATH
# env | grep -i openssl
## quiche = { version = "0.24", default-features = false, features = ["openssl"] }

export PATH=/opt/quictls/bin:$PATH && export LD_LIBRARY_PATH=/opt/quictls/lib:$LD_LIBRARY_PATH && export PKG_CONFIG_PATH=/opt/quictls/lib/pkgconfig:$PKG_CONFIG_PATH && cargo clean && cargo build -r
