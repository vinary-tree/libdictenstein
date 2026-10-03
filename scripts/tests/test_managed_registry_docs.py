"""Offline checks for the exact-version registry documentation verifier."""

from __future__ import annotations

import io
import unittest
import zipfile

from scripts import check_managed_registry_docs as registry


def archive(files: dict[str, bytes]) -> bytes:
    output = io.BytesIO()
    with zipfile.ZipFile(output, mode="w") as zipped:
        for name, content in files.items():
            zipped.writestr(name, content)
    return output.getvalue()


class ManagedRegistryDocsTests(unittest.TestCase):
    def test_maven_javadoc_requires_all_public_types(self) -> None:
        names = registry.API.source_types(
            registry.API.JVM_SOURCE, registry.API.JAVA_TYPE, "java"
        )
        files = {"index.html": b"<html>Javadoc</html>"}
        files.update(
            {
                f"io/vinarytree/libdictenstein/{name}.html": b"<html>API</html>"
                for name in names
            }
        )
        registry.check_maven_archive(archive(files))
        files.pop(f"io/vinarytree/libdictenstein/{min(names)}.html")
        with self.assertRaisesRegex(ValueError, "omits public types"):
            registry.check_maven_archive(archive(files))

    def test_nuget_requires_xml_for_both_target_frameworks(self) -> None:
        names = registry.API.source_types(
            registry.API.DOTNET_SOURCE, registry.API.CSHARP_TYPE, "cs"
        )
        members = "".join(
            f'<member name="T:VinaryTree.Libdictenstein.{name}" />' for name in names
        )
        xml = f"<doc><members>{members}</members></doc>".encode()
        files = {
            "README.md": f"# libdictenstein F# DynamicDawg {registry.VERSION}".encode(),
            "lib/net8.0/VinaryTree.Libdictenstein.xml": xml,
            "lib/net10.0/VinaryTree.Libdictenstein.xml": xml,
        }
        registry.check_nuget_archive(archive(files))
        files.pop("lib/net8.0/VinaryTree.Libdictenstein.xml")
        with self.assertRaisesRegex(ValueError, "lacks lib/net8.0"):
            registry.check_nuget_archive(archive(files))

    def test_spi_must_remain_on_exact_version(self) -> None:
        route = (
            "https://swiftpackageindex.com/vinary-tree/libdictenstein/"
            f"{registry.VERSION}/documentation/libdictenstein"
        )
        registry.check_spi_page(
            f"<html>Libdictenstein {registry.VERSION}</html>".encode(), route
        )
        with self.assertRaisesRegex(ValueError, "redirected away"):
            registry.check_spi_page(
                b"Libdictenstein", route.replace(registry.VERSION, "main")
            )


if __name__ == "__main__":
    unittest.main()
