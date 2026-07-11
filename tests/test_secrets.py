"""Output secret-scan: known credential shapes redacted, clean prose untouched."""

from demerzel.secrets import scan


def test_clean_review_untouched():
    text = "## Summary\nThe change looks correct. Consider adding a test for the edge case."
    res = scan(text)
    assert res.fired is False
    assert res.redactions == 0
    assert res.text == text


def test_aws_access_key_redacted():
    text = "found key AKIAIOSFODNN7EXAMPLE in config"
    res = scan(text)
    assert res.fired is True
    assert "aws_access_key_id" in res.matches
    assert "AKIAIOSFODNN7EXAMPLE" not in res.text
    assert "[REDACTED-SECRET]" in res.text


def test_github_token_redacted():
    text = "token: ghp_" + "a" * 36
    res = scan(text)
    assert res.fired is True
    assert "github_token" in res.matches


def test_gitlab_pat_redacted():
    res = scan("glpat-" + "x" * 20)
    assert res.fired is True
    assert "gitlab_pat" in res.matches


def test_private_key_block_redacted():
    res = scan("-----BEGIN RSA PRIVATE KEY-----\nMIIabc\n-----END RSA PRIVATE KEY-----")
    assert res.fired is True
    assert "private_key_block" in res.matches


def test_generic_secret_assignment():
    res = scan('password = "hunter2hunter2"')
    assert res.fired is True
    assert "generic_secret_assign" in res.matches


def test_jwt_redacted():
    jwt = "eyJ" + "a" * 12 + ".eyJ" + "b" * 12 + "." + "c" * 12
    res = scan(jwt)
    assert res.fired is True
    assert "jwt" in res.matches


def test_high_entropy_blob_redacted():
    # A random-looking base64 blob (high entropy) gets caught by the entropy net.
    blob = "Zm9vYmFyMTIzNDU2Nzg5MEFCQ0RFRkdISUpLTE1OT1BRUlNUVVZ" + "XWFla"
    res = scan(blob)
    assert res.fired is True
    assert "high_entropy" in res.matches


def test_entropy_check_can_be_disabled():
    # low-entropy long word shouldn't fire even with entropy on
    res = scan("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", check_entropy=True)
    assert res.fired is False


def test_trace_shape():
    res = scan("AKIAIOSFODNN7EXAMPLE")
    t = res.to_trace()
    assert t["fired"] is True
    assert "aws_access_key_id" in t["match_kinds"]
    assert t["redactions"] >= 1
