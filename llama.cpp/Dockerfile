FROM ubuntu:22.04

ENV DEBIAN_FRONTEND=noninteractive

RUN apt update && apt upgrade -y && \
    apt install -y --no-install-recommends \
    ca-certificates git build-essential cmake wget curl \
    libcurl4-openssl-dev && \
    apt clean && rm -rf /var/lib/apt/lists/*

WORKDIR /opt
RUN git clone https://github.com/ggml-org/llama.cpp
WORKDIR /opt/llama.cpp

RUN cmake -B build
RUN cmake --build build --config Release -j4

WORKDIR /opt/llama.cpp/build/bin
ENTRYPOINT ["./llama-server"]