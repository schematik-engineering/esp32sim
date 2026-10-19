#!/usr/bin/env python3
"""Unit tests for classification and deliberately incomplete static traversal."""
import unittest
from audit_isa import isa, reachable


def row(m, operands='', length=4, cls=None):
    return dict(mnemonic=m, operands=operands, length=length, isa=cls or isa(m, 0))


class AuditTests(unittest.TestCase):
    def test_extension_classification(self):
        for m, expected in [('mul', 'M'), ('lr.w.aq', 'A'), ('flw', 'F'), ('c.flw', 'Zcf'),
                            ('c.addi', 'C/Zca'), ('c.mul', 'Zcb'), ('cm.push', 'Zcmp'),
                            ('cm.jt', 'Zcmt'), ('sh2add', 'Zba'), ('andn', 'Zbb'),
                            ('bseti', 'Zbs'), ('clmul', 'Zbc'), ('esp.vld.128.ip', 'XespV'),
                            ('fence.i', 'Zifencei'), ('c.unimp', 'unknown/data')]:
            self.assertEqual(isa(m, 0), expected)
        self.assertEqual(isa('csrrw', 0x7c601073), 'XespLoop CSR')
        self.assertEqual(isa('csrrw', 0x30001073), 'Zicsr')

    def test_jump_does_not_fall_into_data(self):
        rows = {0x100000: row('jal', 'zero,100008 <next>'), 0x100004: row('.word'), 0x100008: row('c.jr', 'ra', 2)}
        seen, unresolved = reachable(rows, [0x100000])
        self.assertEqual(seen, {0x100000, 0x100008})
        self.assertFalse(unresolved)

    def test_call_follows_target_and_continuation(self):
        rows = {0x100000: row('jal', 'ra,100008 <func>'), 0x100004: row('c.jr', 'ra', 2), 0x100008: row('c.jr', 'ra', 2)}
        self.assertEqual(reachable(rows, [0x100000])[0], set(rows))

    def test_branch_follows_both_arms(self):
        rows = {0x100000: row('beq', 'a0,a1,100008 <taken>'), 0x100004: row('c.jr', 'ra', 2), 0x100008: row('c.jr', 'ra', 2)}
        self.assertEqual(reachable(rows, [0x100000])[0], set(rows))

    def test_indirect_call_is_not_silently_complete(self):
        rows = {0x100000: row('c.jalr', 'a0', 2), 0x100002: row('c.jr', 'ra', 2)}
        seen, unresolved = reachable(rows, [0x100000])
        self.assertEqual(seen, set(rows))
        self.assertEqual(unresolved, {(0x100000, 'indirect control transfer')})

    def test_auipc_pair_resolves_target(self):
        rows = {0x100000: row('auipc', 'ra,0'), 0x100004: row('jalr', 'ra,8(ra) # 10000c <func>'),
                0x100008: row('c.jr', 'ra', 2), 0x10000c: row('c.jr', 'ra', 2)}
        seen, unresolved = reachable(rows, [0x100000])
        self.assertEqual(seen, set(rows))
        self.assertFalse(unresolved)

    def test_unknown_boundary_is_not_executed_code(self):
        rows = {0x100000: row('.insn'), 0x100004: row('addi')}
        seen, unresolved = reachable(rows, [0x100000])
        self.assertEqual(seen, {0x100000})
        self.assertEqual(unresolved, {(0x100000, 'unknown/data boundary')})


if __name__ == '__main__': unittest.main()
