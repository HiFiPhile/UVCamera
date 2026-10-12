# Keep standalone entry points launched via app_process from the module.
-keep class com.hifiphile.uvcamera.standalone.* {
    public static void main(java.lang.String[]);
}
