FROM ghcr.io/astral-sh/uv:python3.13-trixie

RUN apt update && apt-get install -y ffmpeg git wget

WORKDIR /app

RUN git clone https://github.com/rupeshs/fastsdcpu.git fastsdcpu

WORKDIR /app/fastsdcpu

RUN wget https://huggingface.co/rupeshs/FastSD-Flux-GGUF/blob/main/libstable-diffusion.so

SHELL ["/bin/bash", "-c"]

RUN echo y | bash -x ./install.sh --disable-gui

VOLUME /app/fastsdcpu/lora_models
VOLUME /app/fastsdcpu/controlnet_models
VOLUME /app/fastsdcpu/models/gguf
VOLUME /root/.cache/huggingface

ENV GRADIO_SERVER_NAME=0.0.0.0

EXPOSE 7860

CMD ["/app/fastsdcpu/start-webui.sh"]
