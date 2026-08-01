GET {{baseurl}}/info
HTTP 200
[Asserts]
jsonpath "$.std.name" == "search-searxng"
jsonpath "$.std.version" isString
