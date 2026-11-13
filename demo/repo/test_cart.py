from cart import checkout, total


def test_total():
    assert total([{"price": 5, "qty": 2}, {"price": 1, "qty": 3}]) == 13


def test_checkout_with_items():
    assert checkout({"items": [{"price": 4, "qty": 1}]})["amount"] == 4


def test_checkout_with_empty_cart():
    # Fails before the fix: a cart with no "items" key crashes with
    # TypeError: 'NoneType' object is not iterable.
    assert checkout({})["amount"] == 0
