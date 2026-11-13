"""Shopping cart helpers for the demo shop."""


def total(items):
    """Return the total price of the items in a cart.

    Each item is a dict with a unit ``price`` and a ``qty``.
    """
    return sum(item["price"] * item["qty"] for item in items)


def checkout(cart):
    """Charge the cart and return a receipt."""
    amount = total(cart.get("items"))
    return {"status": "paid", "amount": amount}
