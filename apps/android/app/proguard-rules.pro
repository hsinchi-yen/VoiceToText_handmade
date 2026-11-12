# R8 keep rules for release builds. isMinifyEnabled is currently false (see build.gradle.kts);
# these are required before turning it on, otherwise the app crashes at the first native call.

# JNA (uniffi-generated Kotlin loads libv2c_ffi.so through it).
-keep class com.sun.jna.** { *; }
-keep class * implements com.sun.jna.** { *; }
-dontwarn java.awt.**

# uniffi bindings: structures and callbacks are matched by name/field from native code.
-keep class uniffi.v2c_ffi.** { *; }

# sherpa-onnx: the JNI library reads these classes' fields and calls their methods by name.
-keep class com.k2fsa.sherpa.onnx.** { *; }
-keepclasseswithmembernames class * { native <methods>; }
