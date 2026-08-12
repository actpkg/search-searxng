async def test_component_exposes_its_tools(client):
    """hurl tools.hurl: `$.tools` count >= 1."""
    tools = await client.list_tools()
    assert len(tools) >= 1


async def test_search_is_listed_with_both_mandatory_args_required(client):
    """hurl search.hurl's first block: the search tool is listed, and both
    mandatory arguments are declared as required.
    """
    tools = await client.list_tools()
    assert tools[0].name == "search"
    required = tools[0].inputSchema.get("required", [])
    assert "query" in required
    assert "base_url" in required
