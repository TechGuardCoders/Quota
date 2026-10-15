import pytest

from quota import Denial, Quota


class FakeClock:
    def __init__(self) -> None:
        self.now = 1000.0

    def __call__(self) -> float:
        return self.now

    def advance(self, s: float) -> None:
        self.now += s


def make_quota(limit=1000, window_s=100):
    clock = FakeClock()
    return Quota(limit=limit, window_s=window_s, clock=clock), clock


def test_allow_consumes_within_limit():
    q, _ = make_quota(limit=100)
    assert q.allow("acme", 60) is True
    assert q.usage("acme") == 60
    assert q.remaining("acme") == 40


def test_tenants_are_isolated():
    q, _ = make_quota(limit=100)
    assert q.allow("acme", 100) is True
    assert q.allow("other", 100) is True
    assert q.usage("acme") == 100
    assert q.usage("other") == 100


def test_over_limit_returns_structured_denial():
    q, _ = make_quota(limit=100)
    q.allow("acme", 80)
    result = q.allow("acme", 30)
    assert isinstance(result, Denial)
    assert not result
    assert result.tenant == "acme"
    assert result.requested == 30
    assert result.remaining == 20
    assert result.reason == "quota_exceeded"
    assert result.retry_after > 0


def test_denial_never_raises_or_hangs():
    q, _ = make_quota(limit=10)
    for _ in range(50):
        result = q.allow("acme", 7)
        assert isinstance(result, (bool, Denial))


def test_warn_at_80_percent():
    q, _ = make_quota(limit=100)
    q.allow("acme", 79)
    assert q.warn("acme") is False
    q.allow("acme", 1)
    assert q.warn("acme") is True


def test_warn_false_for_unknown_tenant():
    q, _ = make_quota()
    assert q.warn("nobody") is False


def test_reset_on_window_rollover():
    q, clock = make_quota(limit=100, window_s=60)
    assert q.allow("acme", 100) is True
    assert not q.allow("acme", 1)  # denial
    clock.advance(60)
    assert q.usage("acme") == 0
    assert q.allow("acme", 100) is True


def test_retry_after_counts_down_to_reset():
    q, clock = make_quota(limit=100, window_s=60)
    q.allow("acme", 100)
    result = q.allow("acme", 1)
    assert 0 < result.retry_after <= 60
    clock.advance(30)
    result = q.allow("acme", 1)
    assert 0 < result.retry_after <= 30


def test_request_larger_than_total_limit_rejected():
    q, _ = make_quota(limit=100)
    result = q.allow("acme", 101)
    assert isinstance(result, Denial)
    assert result.reason == "request_exceeds_total_limit"


def test_invalid_config_raises():
    with pytest.raises(ValueError):
        Quota(limit=0, window_s=10)
    with pytest.raises(ValueError):
        Quota(limit=10, window_s=0)


def test_negative_tokens_raises():
    q, _ = make_quota()
    with pytest.raises(ValueError):
        q.allow("acme", -1)


def test_zero_tokens_is_free_but_tracked():
    q, _ = make_quota(limit=100)
    assert q.allow("acme", 0) is True
    assert q.usage("acme") == 0
