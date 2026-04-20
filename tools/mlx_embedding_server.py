#!/usr/bin/env python3
"""
MLX Embedding Server - 本地 MLX 加速的 Embedding 服务。

用法：
    python3 tools/mlx_embedding_server.py --model Qwen/Qwen3-Embedding-0.6B --port 11435

环境：
    pip3 install mlx-embeddings fastapi uvicorn

特性：
    - 兼容 OpenAI /v1/embeddings API 格式
    - 支持 MLX 原生加速（Apple Silicon Metal）
    - 默认模型：Qwen3-Embedding-0.6B（中文特化、1024维、~1.2GB）

Rust 端配置：
    export OPENAI_API_KEY=mlx-local
    export OPENAI_BASE_URL=http://localhost:11435/v1
    export OPENAI_EMBEDDING_MODEL=Qwen3-Embedding-0.6B
"""

import argparse
import time

import mlx.core as mx
import mlx_embeddings
from fastapi import FastAPI
from pydantic import BaseModel
from typing import List, Union
import uvicorn

app = FastAPI(title="MLX Embedding Server")

# 全局模型/ tokenizer
_model = None
_tokenizer = None
_model_name = ""


def load_model(model_id: str):
    global _model, _tokenizer, _model_name
    print(f"[MLX] Loading {model_id}...")
    started = time.time()
    _model, _tokenizer = mlx_embeddings.load(model_id)
    _model_name = model_id
    elapsed = time.time() - started
    print(f"[MLX] Loaded in {elapsed:.2f}s")


class EmbeddingRequest(BaseModel):
    model: str
    input: Union[str, List[str]]
    encoding_format: str = "float"


class EmbeddingData(BaseModel):
    object: str = "embedding"
    embedding: List[float]
    index: int


class EmbeddingResponse(BaseModel):
    object: str = "list"
    data: List[EmbeddingData]
    model: str
    usage: dict


@app.post("/v1/embeddings")
async def embeddings(req: EmbeddingRequest):
    texts = [req.input] if isinstance(req.input, str) else req.input
    if not texts:
        return EmbeddingResponse(
            data=[],
            model=_model_name,
            usage={"prompt_tokens": 0, "total_tokens": 0}
        )

    started = time.time()

    # Tokenize
    inputs = _tokenizer(texts, return_tensors="mlx", padding=True, truncation=True)
    input_ids = inputs["input_ids"]
    attention_mask = inputs.get("attention_mask")

    # Forward
    output = _model(input_ids, attention_mask=attention_mask)
    vectors = output.text_embeds.tolist()

    latency_ms = int((time.time() - started) * 1000)

    data = [
        EmbeddingData(embedding=vec, index=i)
        for i, vec in enumerate(vectors)
    ]

    print(f"[MLX] batch_size={len(texts)} latency={latency_ms}ms dim={len(vectors[0])}")

    return EmbeddingResponse(
        data=data,
        model=_model_name,
        usage={
            "prompt_tokens": sum(len(t) for t in texts),
            "total_tokens": sum(len(t) for t in texts)
        },
    )


@app.get("/health")
async def health():
    return {"status": "ok", "model": _model_name}


@app.get("/")
async def root():
    return {"status": "ok", "model": _model_name, "api_version": "v1"}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="MLX Embedding Server")
    parser.add_argument("--model", default="Qwen/Qwen3-Embedding-0.6B", help="HuggingFace model ID")
    parser.add_argument("--port", type=int, default=11435, help="HTTP port")
    parser.add_argument("--host", default="127.0.0.1", help="HTTP host")
    args = parser.parse_args()

    load_model(args.model)
    uvicorn.run(app, host=args.host, port=args.port)
