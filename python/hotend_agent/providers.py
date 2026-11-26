"""Pydantic AI model objects for the five providers, all wired to the core's transport.

Every provider SDK gets the same `httpx2.AsyncClient`, whose transport is `CoreTransport`, and
the base URL the core named in `agent.run`, so the paths the SDK builds are the ones the
core's egress table allows. The API key given to the SDKs is a placeholder: the transport
drops credential headers and the core attaches the real key.

Provider SDKs are imported only for the provider in use.
"""

from __future__ import annotations

from typing import Any

import httpx2
from pydantic_ai.models import Model
from pydantic_ai.settings import ModelSettings

PROVIDER_KINDS = ("anthropic", "openai", "google", "openrouter", "ollama")

#: What the SDKs are given instead of a key. It never leaves the process.
PLACEHOLDER_KEY = "held-by-hotend-core"

#: Output tokens asked for per request when the run names no limit. The core refuses a
#: request without a limit and one above its ceiling (32 000 by default).
DEFAULT_MAX_OUTPUT_TOKENS = 16_000

#: Seconds without a byte from the core before a model request counts as dead. The core's own
#: limit for a whole request is 600.
READ_TIMEOUT = 660.0


class UnknownProvider(ValueError):
    """`provider.kind` is not one this agent can talk to."""


def http_client(transport: httpx2.AsyncBaseTransport) -> httpx2.AsyncClient:
    """The one HTTP client of a run. Connect and pool timeouts mean nothing without sockets."""
    return httpx2.AsyncClient(transport=transport, timeout=httpx2.Timeout(READ_TIMEOUT))


def build_model(kind: str, model_name: str, base_url: str, client: httpx2.AsyncClient) -> Model:
    """The Pydantic AI model for `provider.kind`, sending through `client`."""
    if kind == "anthropic":
        from pydantic_ai.models.anthropic import AnthropicModel
        from pydantic_ai.providers.anthropic import AnthropicProvider

        return AnthropicModel(model_name, provider=AnthropicProvider(
            api_key=PLACEHOLDER_KEY, base_url=base_url, http_client=client))
    if kind == "openai":
        from pydantic_ai.models.openai import OpenAIResponsesModel
        from pydantic_ai.providers.openai import OpenAIProvider

        return OpenAIResponsesModel(model_name, provider=OpenAIProvider(
            api_key=PLACEHOLDER_KEY, base_url=base_url, http_client=client))
    if kind == "google":
        from pydantic_ai.models.google import GoogleModel
        from pydantic_ai.providers.google import GoogleProvider

        return GoogleModel(model_name, provider=GoogleProvider(
            api_key=PLACEHOLDER_KEY, base_url=base_url, http_client=client))
    if kind == "openrouter":
        from pydantic_ai.models.openrouter import OpenRouterModel
        from pydantic_ai.providers.openrouter import OpenRouterProvider

        return OpenRouterModel(model_name, provider=OpenRouterProvider(
            api_key=PLACEHOLDER_KEY, base_url=base_url, http_client=client))
    if kind == "ollama":
        from pydantic_ai.models.ollama import OllamaModel
        from pydantic_ai.providers.ollama import OllamaProvider

        return OllamaModel(model_name, provider=OllamaProvider(
            api_key=PLACEHOLDER_KEY, base_url=base_url, http_client=client))
    raise UnknownProvider(f"unknown provider kind {kind!r}; expected one of {PROVIDER_KINDS}")


def model_settings(max_output_tokens: Any = None) -> ModelSettings:
    """Settings sent with every request: always an output-token limit."""
    limit = DEFAULT_MAX_OUTPUT_TOKENS
    if isinstance(max_output_tokens, int) and not isinstance(max_output_tokens, bool):
        limit = max(256, min(max_output_tokens, 32_000))
    return ModelSettings(max_tokens=limit)


#: Provider kinds whose web search and web fetch the core can allow (docs/WEB_RESEARCH.md).
RESEARCH_KINDS = ("anthropic",)

#: Lifetimes of a prompt-cache entry the provider offers.
CACHE_TTLS = ("5m", "1h")


def research_capabilities(fetch: bool) -> list[Any]:
    """The provider's web tools, declared for a run with web research.

    They are markers. The core's egress replaces each with an entry it writes itself (tool
    version, caps, domains, which links may be fetched), so nothing set here reaches the
    provider; a run without the core's grant is refused when it sends them.
    """
    from pydantic_ai.capabilities import NativeTool
    from pydantic_ai.native_tools import WebFetchTool, WebSearchTool

    tools: list[Any] = [NativeTool(WebSearchTool(max_uses=1))]
    if fetch:
        tools.append(NativeTool(WebFetchTool(max_uses=1)))
    return tools


def cache_settings(settings: ModelSettings, cache_ttl: str) -> ModelSettings:
    """`settings` with the prompt cache switched on for the tools, the instructions and the
    conversation.

    For every run of a provider whose cache is driven by markers, with research or without:
    the instructions and the tools are sent again with every request, and web results stay
    in a project's conversation after research stopped. Without the markers all of it is
    read at the full price each time.
    """
    ttl = cache_ttl if cache_ttl in CACHE_TTLS else "5m"
    return ModelSettings(**{
        **settings,
        "anthropic_cache_tool_definitions": ttl,
        "anthropic_cache_instructions": ttl,
        "anthropic_cache_messages": ttl,
    })


def research_settings(settings: ModelSettings, cache_ttl: str) -> ModelSettings:
    """`settings` for a run with web research.

    The web tools may only be called by the model directly, and the prompt cache is on (see
    `cache_settings`): the core refuses a research request without a cache marker.
    """
    return ModelSettings(**{
        **cache_settings(settings, cache_ttl),
        "anthropic_web_tools_allowed_callers": ["direct"],
    })
