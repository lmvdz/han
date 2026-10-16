"""The sandbox path form is the same on every OS (R-24 sandbox boundary, V7, han.ts file.write): see han/paths.py."""
import pytest
from han.paths import portable_relative
from han.exec.actions import Actions, Refused

BAD = ["/etc/x", "\\x", "\\\\server\\share\\x", "//server/share/x", "C:\\x", "C:/x", "C:", "c:x", "..", "../x", "..\\x", "a/../x", "a\\..\\x",
       "a/..", "", ".", "C:\n.ipynb", "a\nb", "a\x00b", "a\tb", "a\x7fb"]
GOOD = ["a", "a.txt", "sub/a.txt", "sub\\a.txt", "..a", "a..b", "a/.../b"]

@pytest.mark.parametrize("p", BAD + [None, 5])
def test_refused_everywhere(p):
    assert not portable_relative(p)

@pytest.mark.parametrize("p", GOOD)
def test_relative_names_pass(p):
    assert portable_relative(p)

def test_root_only_where_asked():
    assert portable_relative(".", True) and portable_relative("", True) and not portable_relative("../x", True)

def test_file_write_target_is_checked_by_the_same_rule(tmp_path):
    acts = Actions(str(tmp_path))
    for bad in BAD:
        with pytest.raises(Refused): acts.check({"action": "file.write", "target": bad, "args": {"content": "x"}})
    acts.check({"action": "file.write", "target": "sub\\a.txt", "args": {"content": "x"}})
