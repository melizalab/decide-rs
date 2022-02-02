FROM rustembedded/cross:armv7-unknown-linux-gnueabihf

RUN dpkg --add-architecture armv7-unknown-linux-gnueabihf && \
    apt-get update && \
    apt-get install --assume-yes --no-install-recommends \
    build-essential \
    g++-arm-linux-gnueabihf \
    libc6-dev-armhf-cross \
    libasound2-dev \
    libzmq3-dev \
    pkg-config
ENV RUST_TARGET=armv7-unknown-linux-gnueabihf
ENV PATH=${PATH}:/root/.cargo/bin \
  PKG_CONFIG_PATH=$SYSROOT/usr/lib/arm-linux-gnueabihf/pkgconfig \
  PKG_CONFIG_SYSROOT_DIR=$SYSROOT
ENV CROSS_TRIPLE=arm-linux-gnueabihf
ENV CROSS_COMPILE=${CROSS_TRIPLE}-
ENV CC=${CROSS_COMPILE}gcc
