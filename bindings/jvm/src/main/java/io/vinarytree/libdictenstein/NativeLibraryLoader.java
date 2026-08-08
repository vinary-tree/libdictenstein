package io.vinarytree.libdictenstein;

import java.io.IOException;
import java.io.InputStream;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.util.Locale;

/** Loads the platform library bundled in the Maven artifact. */
final class NativeLibraryLoader {
    private static volatile boolean loaded;

    private NativeLibraryLoader() {}

    static synchronized void load() {
        if (loaded) return;
        String resource = resourcePath(System.getProperty("os.name"), System.getProperty("os.arch"));
        try (InputStream input = NativeLibraryLoader.class.getResourceAsStream(resource)) {
            if (input == null) {
                System.loadLibrary("libdictenstein");
            } else {
                String suffix = resource.substring(resource.lastIndexOf('.'));
                Path extracted = Files.createTempFile("libdictenstein-", suffix);
                extracted.toFile().deleteOnExit();
                Files.copy(input, extracted, StandardCopyOption.REPLACE_EXISTING);
                System.load(extracted.toAbsolutePath().toString());
            }
            loaded = true;
        } catch (IOException error) {
            throw new ExceptionInInitializerError(error);
        }
    }

    static String resourcePath(String osName, String architecture) {
        String os = osName.toLowerCase(Locale.ROOT);
        String arch = switch (architecture.toLowerCase(Locale.ROOT)) {
            case "amd64", "x86_64", "x64" -> "x86_64";
            case "aarch64", "arm64" -> "aarch64";
            default -> throw unsupported(osName, architecture);
        };
        if (os.contains("linux")) return "/META-INF/native/linux-" + arch + "/liblibdictenstein.so";
        if ((os.contains("mac") || os.contains("darwin")) && arch.equals("aarch64")) {
            return "/META-INF/native/macos-aarch64/liblibdictenstein.dylib";
        }
        if (os.contains("windows") && arch.equals("x86_64")) {
            return "/META-INF/native/windows-x86_64/libdictenstein.dll";
        }
        throw unsupported(osName, architecture);
    }

    private static UnsupportedOperationException unsupported(String os, String arch) {
        return new UnsupportedOperationException(
                "unsupported libdictenstein JVM platform: " + os + " / " + arch);
    }
}
