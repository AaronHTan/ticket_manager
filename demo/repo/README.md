# web-app (demo)

A deliberately tiny shop used by `scripts/demo.sh`. `cart.py` contains a
planted bug: checking out a cart with no items crashes with
`TypeError: 'NoneType' object is not iterable`, which the web layer reports
as a 500 error. `test_cart.py::test_checkout_with_empty_cart` shows it.

The ticket-handler demo fixes it automatically by re-applying the patch
stored with a previously resolved ticket.
