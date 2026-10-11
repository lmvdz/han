"""Event types and shapes of the Matrix binding (§8.2), shared by the appservice and the client."""
ENTRY, RECEIPT, REJECT = "org.han.entry", "org.han.receipt", "org.han.reject"
SPACE, SESSION, STREAM, SIGNAL = "org.han.space", "org.han.session", "org.han.stream", "org.han.signal"
SERVICE_LOCALPART = "han-seq"

def relates(event_id: str) -> dict:
    """The relation that ties a receipt or reject to the entry's Matrix event."""
    return {"rel_type": "m.reference", "event_id": event_id}

def entry_content(entry: dict, content=None) -> dict:
    return {"entry": entry} | ({} if content is None else {"content": content})

def service_user(server: str) -> str: return f"@{SERVICE_LOCALPART}:{server}"

def txn_safe(event_id: str) -> str:
    return event_id.lstrip("$")
